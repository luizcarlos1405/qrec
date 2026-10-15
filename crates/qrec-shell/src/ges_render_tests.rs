//! GES vs frame-plan parity (A§11, SPEC §12): a two-layer take rendered by
//! [`FrameRenderer`] and by the ffmpeg frame plan must agree per layer rect
//! (SSIM ≥ 0.98), and a frame-ID layer (the source frame index drawn as
//! binary bars) must show the exact source frame in both renderers.
//!
//! The plans come from the core (`plan_ges`, `plan_frame`). While those are
//! still stubs (`todo!()` before the output owner's work is merged), the
//! test builds the same plans by hand from A§9.3, A§9.4 and A§9.6, so the
//! renderer is checked either way.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use qrec_core::hash::sha256_hex;
use qrec_core::layout::place;
use qrec_core::model::{Project, Rect, StreamKind};
use qrec_core::plans::{GesAudioClip, GesTimeline, GesVideoClip};
use qrec_core::time::frame_ns;
use qrec_core::{Fps, TakeId};
use serde_json::json;

use super::*;
use crate::gst_init::testing::{assert_quiet_since, problems, serial};
use crate::testkit::{MediaSpec, TestEnv, fixture, fixtures_dir, run_tool, write_project};

const FFMPEG_TIMEOUT: Duration = Duration::from_secs(120);
const CANVAS: (u32, u32) = (320, 180);
const FPS: Fps = Fps::new(30, 1);
const TAKE_FRAMES: u64 = 90;
/// Bars of the frame-ID video: bit `i` of the frame index, left to right.
const ID_BITS: u32 = 8;
/// The clips: take frames `[10, 40)`, then `[55, 80)` and `[80, 90)`, which
/// are continuous (one render unit) and end with the take.
const CLIPS: [(u64, u64); 3] = [(10, 40), (55, 80), (80, 90)];

/// `frames` frames whose luma shows the frame index as `ID_BITS` vertical
/// bars (white = 1), H.264 limited-range BT.709 in Matroska (millisecond
/// timestamps, like recordings). Cached next to the testkit fixtures.
fn frame_id_fixture(width: u32, height: u32, fps: Fps, frames: u64) -> PathBuf {
    let key = format!(
        "qrec frame-id v1 {width}x{height} {}/{} {frames}",
        fps.num, fps.den
    );
    let path = fixtures_dir().join(format!("{}.mkv", sha256_hex(key.as_bytes())));
    if path.is_file() {
        return path;
    }
    let tmp = path.with_extension(format!("{}.part.mkv", std::process::id()));
    let graph = format!(
        "nullsrc=size={width}x{height}:rate={}/{},trim=end_frame={frames},setpts=PTS-STARTPTS,\
         format=yuv420p,geq=lum='if(bitand(N\\,pow(2\\,floor(X*{ID_BITS}/W)))\\,235\\,16)':cb=128:cr=128,\
         setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709[v]",
        fps.num, fps.den
    );
    let gop = (fps.num / fps.den).max(1).to_string();
    let out = run_tool(
        Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostdin",
                "-nostats",
                "-loglevel",
                "error",
                "-y",
            ])
            .args(["-filter_complex", &graph, "-map", "[v]"])
            .args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-bf", "0",
            ])
            .args(["-g", &gop, "-pix_fmt", "yuv420p", "-color_range", "tv"])
            .args([
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
            ])
            .args(["-f", "matroska"])
            .arg(format!("file:{}", tmp.display())),
        FFMPEG_TIMEOUT,
    );
    assert!(out.status.success(), "frame-ID fixture: {}", out.stderr);
    fs::rename(&tmp, &path).unwrap();
    path
}

/// A PNG decoded to packed RGB with ffmpeg.
struct Rgb {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Rgb {
    fn load(png: &Path, width: u32, height: u32) -> Self {
        let out = run_tool(
            Command::new("ffmpeg")
                .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"])
                .arg(format!("file:{}", png.display()))
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]),
            FFMPEG_TIMEOUT,
        );
        assert!(
            out.status.success(),
            "decode {}: {}",
            png.display(),
            out.stderr
        );
        assert_eq!(
            out.stdout.len(),
            (width * height * 3) as usize,
            "{} size",
            png.display()
        );
        Self {
            width,
            height,
            data: out.stdout,
        }
    }

    fn red(&self, x: u32, y: u32) -> u8 {
        assert!(x < self.width && y < self.height);
        self.data[((y * self.width + x) * 3) as usize]
    }
}

/// The index drawn by the frame-ID layer at `rect`, read from bar centers.
fn read_frame_id(red: impl Fn(u32, u32) -> u8, rect: Rect) -> u64 {
    let y = rect.y as u32 + rect.h / 2;
    (0..ID_BITS).fold(0, |id, bit| {
        let x = rect.x as u32 + ((2 * bit + 1) * rect.w) / (2 * ID_BITS);
        id | (u64::from(red(x, y) > 128) << bit)
    })
}

/// ffmpeg's SSIM ("All") of two PNGs inside `rect`, measured like video:
/// on BT.709 limited-range YUV 4:4:4 (in RGB, the chroma edges that 4:2:0
/// compositing and 4:4:4 compositing draw differently dominate the score).
fn ssim(a: &Path, b: &Path, rect: Rect) -> f64 {
    let crop = format!(
        "crop={}:{}:{}:{},scale=out_color_matrix=bt709:out_range=tv,format=yuv444p",
        rect.w, rect.h, rect.x, rect.y
    );
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-nostats", "-i"])
            .arg(format!("file:{}", a.display()))
            .arg("-i")
            .arg(format!("file:{}", b.display()))
            .args([
                "-filter_complex",
                &format!("[0:v]{crop}[a];[1:v]{crop}[b];[a][b]ssim"),
            ])
            .args(["-f", "null", "-"]),
        FFMPEG_TIMEOUT,
    );
    assert!(out.status.success(), "ssim: {}", out.stderr);
    let all = out
        .stderr
        .lines()
        .find_map(|l| l.split("All:").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|v| v.parse().ok());
    all.unwrap_or_else(|| panic!("no SSIM in:\n{}", out.stderr))
}

/// A take with a testsrc2 screen (L1, cropped, 80 % opacity, bottom) and a
/// frame-ID camera (L2, top), placed on the timeline as two clips.
struct ParityProject {
    _env: TestEnv,
    dir: PathBuf,
    project: Project,
}

fn parity_project() -> ParityProject {
    let env = TestEnv::new();
    let dir = env.empty_dir("parity");
    let take_dir = dir.join("takes/take-0001");
    fs::create_dir_all(&take_dir).unwrap();
    let screen = fixture(&MediaSpec::video(640, 360, 30, TAKE_FRAMES));
    let camera = frame_id_fixture(160, 120, FPS, TAKE_FRAMES);
    fs::copy(&screen, take_dir.join("L1-screen-FAKE-1.mkv")).unwrap();
    fs::copy(&camera, take_dir.join("L2-camera-0.mkv")).unwrap();

    let layer = |id: &str,
                 source: serde_json::Value,
                 rect: [u32; 4],
                 crop: [u32; 4],
                 fit: &str,
                 opacity: f64| {
        json!({
            "id": id, "name": id, "visible": true, "opacity": opacity, "source": source,
            "rect": { "x": rect[0], "y": rect[1], "w": rect[2], "h": rect[3] },
            "crop": { "top": crop[0], "right": crop[1], "bottom": crop[2], "left": crop[3] },
            "fit": fit,
        })
    };
    let layers = json!([
        layer(
            "L1",
            json!({ "kind": "screen", "output": "FAKE-1", "label": "Fake screen 1" }),
            [0, 0, CANVAS.0, CANVAS.1],
            [20, 40, 20, 40],
            "contain",
            0.8
        ),
        layer(
            "L2",
            json!({ "kind": "camera", "device": "/dev/fake0", "label": "Fake camera" }),
            [200, 100, 100, 75],
            [0, 0, 0, 0],
            "cover",
            1.0
        ),
    ]);
    let stream = |id: &str, file: &str, w: u32, h: u32| {
        json!({
            "id": id, "kind": "video", "file": format!("takes/take-0001/{file}"),
            "width": w, "height": h, "startOffsetNs": 0,
            "durationNs": frame_ns(TAKE_FRAMES as i64, FPS), "frames": TAKE_FRAMES,
            "fps": { "num": FPS.num, "den": FPS.den },
        })
    };
    let mut position = 0;
    let clips: Vec<serde_json::Value> = CLIPS
        .iter()
        .enumerate()
        .map(|(i, &(in_, out))| {
            position += out - in_;
            json!({ "id": format!("c{:03x}", i + 1), "take": "take-0001", "in": in_, "out": out })
        })
        .collect();
    let value = json!({
        "version": 1,
        "name": "parity",
        "createdAt": "2026-10-14T12:00:00Z",
        "canvas": {
            "width": CANVAS.0, "height": CANVAS.1,
            "fps": { "num": FPS.num, "den": FPS.den }, "background": "#12170F",
        },
        "scene": { "layers": layers.clone(), "audio": [] },
        "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },
        "autocut": {
            "autoRunAfterTake": false, "track": null, "thresholdDb": -35.0,
            "minSilenceMs": 400, "paddingMs": 120, "minKeepMs": 200,
        },
        "takes": {
            "take-0001": {
                "status": "ok", "recordedAt": "2026-10-14T12:00:00Z",
                "durationFrames": TAKE_FRAMES,
                "scene": { "layers": layers, "audio": [] },
                "streams": [
                    stream("L1", "L1-screen-FAKE-1.mkv", 640, 360),
                    stream("L2", "L2-camera-0.mkv", 160, 120),
                ],
                "placed": true,
            }
        },
        "nextTakeNumber": 2,
        "timeline": { "clips": clips },
    });
    assert_eq!(position, 65);
    write_project(&dir, &value);
    let project: Project = serde_json::from_value(value).expect("the parity project is valid");
    ParityProject {
        _env: env,
        dir,
        project,
    }
}

/// `(take frame, timeline position, frames)` of each render unit: maximal
/// runs of continuous clips (A§9.3; one take here).
fn units(project: &Project) -> Vec<(u64, u64, u64)> {
    let mut position = 0;
    let mut units: Vec<(u64, u64, u64)> = Vec::new();
    for clip in &project.timeline.clips {
        let n = clip.out - clip.in_;
        match units.last_mut() {
            Some(last) if last.0 + last.2 == clip.in_ => last.2 += n,
            _ => units.push((clip.in_, position, n)),
        }
        position += n;
    }
    units
}

/// The take frame under timeline frame `at`.
fn take_frame(project: &Project, at: u64) -> u64 {
    units(project)
        .into_iter()
        .find(|&(_, pos, n)| at >= pos && at < pos + n)
        .map(|(in_, pos, _)| in_ + at - pos)
        .expect("a frame on the timeline")
}

/// Each visible layer of the take with its placement (z-order, bottom first).
fn placements(project: &Project) -> Vec<(String, String, Placement, f64)> {
    let take = project.take(&TakeId::numbered(1)).unwrap();
    take.scene
        .layers
        .iter()
        .filter(|l| l.visible)
        .filter_map(|layer| {
            let stream = take.streams.iter().find(|s| s.id == layer.id.as_str())?;
            assert_eq!(stream.kind, StreamKind::Video);
            let p = place(
                stream.width?,
                stream.height?,
                layer.crop,
                layer.rect,
                layer.fit,
            )?;
            Some((layer.id.to_string(), stream.file.clone(), p, layer.opacity))
        })
        .collect()
}

/// A§9.6 by hand: one GES clip per layer per render unit (no lead, equal rates).
fn reference_ges(project: &Project) -> GesTimeline {
    let fps = project.canvas.fps;
    let layers = placements(project);
    let top = layers.len() - 1;
    let mut video = Vec::new();
    for (in_, pos, n) in units(project) {
        for (index, (_, file, placement, alpha)) in layers.iter().enumerate() {
            let start = frame_ns(pos as i64, fps) as u64;
            video.push(GesVideoClip {
                file: file.clone(),
                layer_priority: (top - index) as u32,
                start_ns: start,
                inpoint_ns: frame_ns(in_ as i64, fps) as u64,
                duration_ns: frame_ns((pos + n) as i64, fps) as u64 - start,
                placement: *placement,
                alpha: *alpha,
            });
        }
    }
    let total: u64 = units(project).iter().map(|u| u.2).sum();
    GesTimeline {
        canvas: project.canvas.clone(),
        duration_ns: frame_ns(total as i64, fps) as u64,
        video,
        audio: Vec::<GesAudioClip>::new(),
    }
}

/// A§9.3 + A§9.4 by hand: the one-frame composite of timeline frame `at`.
fn reference_frame_argv(project: &Project, at: u64, out: &Path) -> Vec<String> {
    let canvas = &project.canvas;
    let (fps, t) = (canvas.fps, take_frame(project, at));
    let mut argv: Vec<String> = [
        "ffmpeg",
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "info",
        "-y",
    ]
    .map(String::from)
    .to_vec();
    let mut chains = vec![format!(
        "color=c={}:s={}x{}:r={}/{}[base]",
        canvas.background.as_str(),
        canvas.width,
        canvas.height,
        fps.num,
        fps.den
    )];
    let layers = placements(project);
    for (i, (_, file, p, opacity)) in layers.iter().enumerate() {
        let seek = frame_ns(t as i64, fps) - 2_000_000;
        if seek > 0 {
            argv.extend(["-ss".into(), format!("{:.6}", seek as f64 / 1e9)]);
        }
        argv.extend(["-i".into(), format!("file:{file}")]);
        let alpha = if *opacity < 1.0 {
            format!(",lutyuv=a=val*{opacity}")
        } else {
            String::new()
        };
        chains.push(format!(
            "[{i}:v]fps={}/{},setpts=PTS-STARTPTS,crop={}:{}:{}:{},scale={}:{}:flags=bilinear,format=yuva420p{alpha}[l{i}]",
            fps.num, fps.den, p.src.w, p.src.h, p.src.x, p.src.y, p.dst.w, p.dst.h
        ));
    }
    let mut below = "base".to_owned();
    for (i, (_, _, p, _)) in layers.iter().enumerate() {
        chains.push(format!(
            "[{below}][l{i}]overlay=x={}:y={}:eof_action=pass[o{i}]",
            p.dst.x, p.dst.y
        ));
        below = format!("o{i}");
    }
    chains.push(format!(
        "[{below}]trim=end_frame=1,setsar=1,format=yuv420p,\
         scale=out_color_matrix=bt709:out_range=pc,format=rgb24,setparams=color_trc=iec61966-2-1[v]"
    ));
    argv.extend(["-filter_complex".into(), chains.join(";")]);
    argv.extend(
        [
            "-map",
            "[v]",
            "-frames:v",
            "1",
            "-f",
            "image2",
            "-update",
            "1",
        ]
        .map(String::from),
    );
    argv.push(format!("file:{}", out.display()));
    argv
}

/// Copies the rendered frames out of the temp env (it is deleted on drop)
/// to `<target>/qrec-parity-<pid>/` so a failure can be looked at.
fn keep_for_inspection(frames: &Path) -> PathBuf {
    let kept = crate::testkit::target_dir().join(format!("qrec-parity-{}", std::process::id()));
    let _ = fs::create_dir_all(&kept);
    for entry in fs::read_dir(frames).into_iter().flatten().flatten() {
        let _ = fs::copy(entry.path(), kept.join(entry.file_name()));
    }
    kept
}

/// A core planner's result, or `None` while it is still a stub.
fn core<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

fn ges_plan(project: &Project) -> (GesTimeline, &'static str) {
    match core(|| qrec_core::ges_plan::plan_ges(project)) {
        Some(plan) => (plan, "core plan_ges"),
        None => (reference_ges(project), "hand-built A§9.6 plan"),
    }
}

/// Whether the core's `plan_frame` is implemented (asked once).
fn core_frame_plan(project: &Project, out: &Path) -> bool {
    core(|| qrec_core::frame_plan::plan_frame(project, 0, None, &out.to_string_lossy())).is_some()
}

fn frame_argv(project: &Project, at: u64, out: &Path, use_core: bool) -> Vec<String> {
    if !use_core {
        return reference_frame_argv(project, at, out);
    }
    match qrec_core::frame_plan::plan_frame(project, at, None, &out.to_string_lossy()) {
        Ok(plan) => plan.argv,
        Err(e) => panic!("plan_frame({at}): {e}"),
    }
}

#[test]
fn ges_matches_the_frame_plan_and_shows_exact_frames() {
    let _serial = serial();
    let quiet = problems();
    let p = parity_project();
    let (plan, origin) = ges_plan(&p.project);
    let layers = placements(&p.project);
    let camera_rect = layers.iter().find(|l| l.0 == "L2").unwrap().2.dst;
    let out = p.dir.join("frames");
    fs::create_dir(&out).unwrap();
    let use_core = core_frame_plan(&p.project, &out.join("probe.png"));
    eprintln!(
        "parity: GES from the {origin}, ffmpeg from the {}",
        if use_core {
            "core plan_frame"
        } else {
            "hand-built A§9.4 plan"
        }
    );

    // Every rounding phase of 30 fps in Matroska (frame k at round(k·33.3) ms),
    // both units, their first and last frames, a cut inside a unit and the
    // take's very last frame.
    let frames = [0u64, 1, 2, 3, 29, 30, 31, 32, 54, 55, 63, 64];
    let mut renderer = FrameRenderer::new(&plan, &p.dir).expect("a GES renderer");
    assert_eq!(renderer.frame_count(), 65);
    for &at in &frames {
        let expected = take_frame(&p.project, at);
        let ges_png = out.join(format!("ges-{at}.png"));
        let frame = renderer.render(at).unwrap();
        assert_eq!((frame.width, frame.height), CANVAS);
        let ges_id = read_frame_id(|x, y| frame.pixel(x, y).unwrap()[0], camera_rect);
        fs::write(&ges_png, frame.png().unwrap()).unwrap();

        let ffmpeg_png = out.join(format!("ffmpeg-{at}.png"));
        let argv = frame_argv(&p.project, at, &ffmpeg_png, use_core);
        let run = run_tool(
            Command::new(&argv[0]).args(&argv[1..]).current_dir(&p.dir),
            FFMPEG_TIMEOUT,
        );
        assert!(run.status.success(), "ffmpeg frame {at}: {}", run.stderr);
        let ffmpeg = Rgb::load(&ffmpeg_png, CANVAS.0, CANVAS.1);
        let ffmpeg_id = read_frame_id(|x, y| ffmpeg.red(x, y), camera_rect);

        assert_eq!(
            (ges_id, ffmpeg_id),
            (expected, expected),
            "timeline frame {at}: (GES, ffmpeg) show source frames ({ges_id}, {ffmpeg_id})"
        );
        for (id, _, placement, _) in &layers {
            let score = ssim(&ges_png, &ffmpeg_png, placement.dst);
            eprintln!("parity: frame {at} (source frame {expected}), layer {id}: SSIM {score:.4}");
            if score < 0.98 {
                let kept = keep_for_inspection(&out);
                panic!(
                    "frame {at}, layer {id}: SSIM {score} < 0.98 (frames kept in {})",
                    kept.display()
                );
            }
        }
    }
    // Never past the end.
    assert!(matches!(renderer.render(65), Err(GesError::Render(_))));
    drop(renderer);
    assert_quiet_since(quiet);
}

/// A camera recorded at 25 fps on a 30 fps canvas, in units starting
/// anywhere (one frame long, trimmed by a frame, over an earlier range):
/// GES shows at every timeline frame the recorded frame the export shows,
/// the nearest to the frame's take time and the earlier one on a tie
/// (A§9.3, A§9.6), not one shifted by where the unit starts.
#[test]
fn ges_shows_the_nearest_frame_of_another_recorded_rate() {
    let _serial = serial();
    let quiet = problems();
    let env = TestEnv::new();
    let dir = env.empty_dir("rates");
    let take_dir = dir.join("takes/take-0001");
    fs::create_dir_all(&take_dir).unwrap();
    let rate = Fps::new(25, 1);
    let recorded = 250;
    fs::copy(
        frame_id_fixture(160, 120, rate, recorded),
        take_dir.join("L1-camera-0.mkv"),
    )
    .unwrap();
    let layers = json!([{
        "id": "L1", "name": "L1", "visible": true, "opacity": 1.0,
        "source": { "kind": "camera", "device": "/dev/fake0", "label": "Fake camera" },
        "rect": { "x": 0, "y": 0, "w": CANVAS.0, "h": CANVAS.1 },
        "crop": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
        "fit": "contain",
    }]);
    let ranges = [(0u64, 20u64), (101, 112), (202, 203), (204, 215), (3, 9)];
    let clips: Vec<serde_json::Value> = ranges
        .iter()
        .enumerate()
        .map(|(i, &(in_, out))| {
            json!({ "id": format!("c{:03x}", i + 1), "take": "take-0001", "in": in_, "out": out })
        })
        .collect();
    let value = json!({
        "version": 1,
        "name": "rates",
        "createdAt": "2026-10-14T12:00:00Z",
        "canvas": {
            "width": CANVAS.0, "height": CANVAS.1,
            "fps": { "num": FPS.num, "den": FPS.den }, "background": "#12170F",
        },
        "scene": { "layers": layers.clone(), "audio": [] },
        "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },
        "autocut": {
            "autoRunAfterTake": false, "track": null, "thresholdDb": -35.0,
            "minSilenceMs": 400, "paddingMs": 120, "minKeepMs": 200,
        },
        "takes": {
            "take-0001": {
                "status": "ok", "recordedAt": "2026-10-14T12:00:00Z",
                "durationFrames": 300,
                "scene": { "layers": layers, "audio": [] },
                "streams": [{
                    "id": "L1", "kind": "video", "file": "takes/take-0001/L1-camera-0.mkv",
                    "width": 160, "height": 120, "startOffsetNs": 0,
                    "durationNs": frame_ns(recorded as i64, rate), "frames": recorded,
                    "fps": { "num": rate.num, "den": rate.den },
                }],
                "placed": true,
            }
        },
        "nextTakeNumber": 2,
        "timeline": { "clips": clips },
    });
    write_project(&dir, &value);
    let project: Project = serde_json::from_value(value).expect("a valid project");
    let rect = placements(&project)[0].2.dst;
    let plan = qrec_core::ges_plan::plan_ges(&project);
    let mut renderer = FrameRenderer::new(&plan, &dir).expect("a GES renderer");
    let mut at = 0;
    let mut wrong = Vec::new();
    for &(in_, out) in &ranges {
        for f in in_..out {
            let frame = renderer.render(at).unwrap();
            let shown = read_frame_id(|x, y| frame.pixel(x, y).unwrap()[0], rect);
            let expected = qrec_core::export_plan::shown_frame(rate, 0, FPS, f as i64);
            if shown as i64 != expected {
                wrong.push(format!(
                    "timeline {at} (take frame {f}): {shown}, not {expected}"
                ));
            }
            at += 1;
        }
    }
    assert!(
        wrong.is_empty(),
        "GES showed other frames:\n{}",
        wrong.join("\n")
    );
    drop(renderer);
    assert_quiet_since(quiet);
}

/// The peak of `samples` (interleaved F32 stereo) in frames `[from, to)`.
fn peak(samples: &[f32], from: usize, to: usize) -> f32 {
    samples[from * 2..to * 2]
        .iter()
        .fold(0.0, |m, s| m.max(s.abs()))
}

/// The first `frames` audio frames the timeline plays (F32LE stereo, 48 kHz).
fn play_audio(timeline: &ges::Timeline, frames: usize) -> Vec<f32> {
    let pipeline = preview_pipeline(timeline, ges::PipelineFlags::FULL_PREVIEW).unwrap();
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", AUDIO_RATE as i32)
        .field("channels", 2)
        .field("layout", "interleaved")
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(false).build();
    let video = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    pipeline.preview_set_audio_sink(Some(&sink));
    pipeline.preview_set_video_sink(Some(&video));
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut samples = Vec::new();
    while samples.len() < frames * 2 {
        let sample = sink
            .try_pull_sample(gst::ClockTime::from_seconds(20))
            .expect("audio within 20 s");
        let map = sample.buffer().unwrap().map_readable().unwrap();
        samples.extend(
            map.as_slice()
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
    }
    assert!(set_null_with_deadline(pipeline.upcast(), TEARDOWN_TIMEOUT));
    samples
}

#[test]
fn audio_clips_play_with_their_volume_and_fades() {
    let _serial = serial();
    let quiet = problems();
    let env = TestEnv::new();
    let dir = env.empty_dir("audio");
    // A 440 Hz tone at 0.5 the whole time.
    let tone = fixture(&MediaSpec::audio(48_000, 1, 3000, &[(0, 3000)]));
    fs::copy(&tone, dir.join("A1-mic.mka")).unwrap();
    let canvas: qrec_core::model::Canvas = serde_json::from_value(json!({
        "width": 64, "height": 36, "fps": { "num": 30, "den": 1 }, "background": "#000000",
    }))
    .unwrap();
    let second = 1_000_000_000;
    let faded = GesAudioClip {
        file: "A1-mic.mka".into(),
        start_ns: 0,
        inpoint_ns: second / 2,
        duration_ns: second,
        volume: 0.5,
        fade_in_samples: 240,
        fade_out_samples: 240,
    };
    let plan = GesTimeline {
        canvas,
        duration_ns: 2 * second,
        video: vec![],
        audio: vec![
            faded.clone(),
            // Overlaps the first: it needs a second audio layer.
            GesAudioClip {
                start_ns: second / 2,
                inpoint_ns: 0,
                volume: 0.25,
                fade_in_samples: 0,
                fade_out_samples: 0,
                ..faded.clone()
            },
        ],
    };
    let timeline = build_timeline(&plan, &dir).unwrap();
    let audio: Vec<ges::TrackElement> = timeline
        .layers()
        .iter()
        .flat_map(|layer| layer.clips())
        .filter_map(|clip| {
            clip.find_track_element(None::<&ges::Track>, ges::AudioSource::static_type())
        })
        .collect();
    assert_eq!(audio.len(), 2);
    assert_ne!(
        audio[0].layer_priority(),
        audio[1].layer_priority(),
        "overlapping clips share a layer"
    );

    // The fades: a control binding in the clip's internal (media) time.
    let binding = audio[0].control_binding("volume").expect("a fade binding");
    let at = |ns: u64| -> f64 {
        ControlBindingExt::value(&binding, gst::ClockTime::from_nseconds(ns))
            .unwrap()
            .get()
            .unwrap()
    };
    let ramp = samples_ns(240);
    assert_eq!(at(second / 2), 0.0);
    assert!((at(second / 2 + ramp / 2) - 0.25).abs() < 0.01);
    assert!((at(second / 2 + ramp) - 0.5).abs() < 1e-9);
    assert!((at(second + second / 2 - ramp) - 0.5).abs() < 1e-9);
    assert_eq!(at(second + second / 2), 0.0);
    assert!(audio[1].control_binding("volume").is_none());
    let volume: f64 = TrackElementExt::child_property(&audio[1], "volume")
        .unwrap()
        .get()
        .unwrap();
    assert_eq!(volume, 0.25);

    // What plays: silence-to-tone over the first 240 samples, then the tone
    // at 0.5 × 0.5; from 0.5 s the second clip adds 0.5 × 0.25.
    let samples = play_audio(&timeline, 48_000 * 3 / 4);
    assert!(
        peak(&samples, 0, 24) < 0.06,
        "fade-in start {}",
        peak(&samples, 0, 24)
    );
    let steady = peak(&samples, 4_800, 9_600);
    assert!((steady - 0.25).abs() < 0.02, "steady {steady}");
    let mixed = peak(&samples, 26_400, 31_200);
    assert!((mixed - 0.375).abs() < 0.03, "mixed {mixed}");
    assert_quiet_since(quiet);
}
