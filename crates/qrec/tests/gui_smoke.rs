//! The end-to-end smoke of the window (docs/GUI.md G§8.5, SPEC §17): the
//! GUI host on `design_fixture(Smoke)` in a nested headless sway, with the
//! fake capture backend, steps 1–10 in one run (WP0b wrote 1–3 and 10, WP10
//! the rest).
//!
//! Throughout: no `Crashed`, no `ERROR` line in the GUI log, no GLib or GTK
//! critical, no panic caught, and the a11y walk is empty. The main-loop
//! probe (`QREC_GUI_PROBE=1`, G§8.6) runs the whole time and every stall
//! over 50 ms is reported, with where the main thread's time went (on a
//! CPU, waiting for one, asleep); under `QREC_TEST_WLR=gles2` one while the
//! steps run fails the case ([`GLES2_STALL_MS`]; with pixman the
//! compositor, llvmpipe and ffmpeg share the CPU), and so does a GL program
//! compiled once the window settled (`GSK_DEBUG=shaders`: the GPU warm-up
//! compiles them while the project opens). The stalls before the window
//! first settles (the project opening) are reported, never failed. The
//! probe's report and the steps' times go to
//! `tmp/gui-shots/smoke/smoke-measurements.json`.
//! `QREC_SMOKE_BIN=<path>` runs the steps' window from another binary (the
//! package's, `result/bin/qrec`: the probe's numbers are the build's).
//!
//! The packaged build (G§6.4, WP10) runs the same way:
//! `the_packaged_build_opens_the_smoke_fixture` launches `nix build`'s
//! `result/bin/qrec` (or `$QREC_PACKAGE/bin/qrec`) with none of the dev
//! shell's plugin, schema or font paths and no tools on `PATH`, so the
//! wrapper must supply them all; without a package it says so and passes.
//!
//! `cargo test -p qrec --test gui_smoke -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon; `nix build`
//! first for the packaged case).

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, Launch, MERGED_REGIONS, Nested, Options, Renderer, assert_live_picture_drawn_at,
    assert_regions, count_near, png_size, repo, token_rgb, wait_for_gui_host,
};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use qrec_shell::testkit::{ffprobe, run_tool};
use serde_json::{Value, json};

const AREA: &str = "smoke";

/// Under gles2, a main-loop stall longer than this while the steps run
/// fails the smoke (G§8.5). Until the perf lane it was 400 ms: forks of
/// the window for each ffprobe and ffmpeg held its main thread on malloc's
/// locks (60–215 ms at a take's landing and an export's start), and GL
/// programs compiled on first use (50–90 ms each, the test's shader cache
/// cold); children now start without a fork and the programs compile while
/// the window opens (G§8.6).
const GLES2_STALL_MS: f64 = 50.0;

/// The GL programs GSK compiled so far, in order, from the window's output
/// under `GSK_DEBUG=shaders` (`Gsk-Message: 23:36:15.429: vertex 1,
/// gskgpuborder:`, then the source with its defines): `gskgpuborder (flags
/// 0u, color states 4u, variation 0u)` each, what `gpu_warmup` must draw.
fn compiled_programs(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let mut programs = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let Some(name) = line
            .strip_prefix("Gsk-Message: ")
            .and_then(|rest| rest.split_once(": "))
            .and_then(|(_, what)| what.strip_prefix("vertex "))
            .and_then(|what| what.strip_suffix(':'))
            .and_then(|what| what.split_once(", "))
            .map(|(_, name)| name)
        else {
            continue;
        };
        // Its defines, in the source that follows (up to the next message).
        let define = |key: &str| {
            lines[at + 1..]
                .iter()
                .take_while(|l| !l.starts_with("Gsk-Message: "))
                .find_map(|l| {
                    l.split_once(&format!("#define {key} "))
                        .map(|(_, v)| v.trim())
                })
                .unwrap_or("?")
        };
        programs.push(format!(
            "{name} (flags {}, color states {}, variation {})",
            define("GSK_FLAGS"),
            define("GSK_COLOR_STATES"),
            define("GSK_VARIATION")
        ));
    }
    programs
}

/// The design fixture's canvas: 1920×1080 at 30 fps (BRIEF §3).
const CANVAS: (u64, u64) = (1920, 1080);
const FPS: &str = "30/1";

/// The ids of the timeline's clips, in order.
fn clip_ids(nested: &Nested, project: &Path) -> Vec<String> {
    let list = nested.cli_json(project, &["clip", "list"]);
    list["data"]
        .as_array()
        .or_else(|| list["data"]["clips"].as_array())
        .unwrap_or_else(|| panic!("clip list: {list}"))
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn clip_count(nested: &Nested, project: &Path) -> usize {
    clip_ids(nested, project).len()
}

/// `qrec status --json`'s data.
fn status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

fn phase(nested: &Nested, project: &Path) -> String {
    status(nested, project)["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Polls `qrec status` until the record phase is `want`.
fn wait_phase(nested: &Nested, project: &Path, want: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let now = phase(nested, project);
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "never {want} (still {now})");
        thread::sleep(Duration::from_millis(100));
    }
}

/// The headless hosts running in the env (`qrec host --headless …`): a
/// routed request spawns none.
fn headless_hosts(nested: &Nested) -> Vec<String> {
    let marker = format!("QREC_RUNTIME_DIR={}", nested.env.runtime_dir().display());
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !environ
            .split(|&b| b == 0)
            .any(|var| var == marker.as_bytes())
        {
            continue;
        }
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if cmdline.contains("--headless") {
            found.push(cmdline);
        }
    }
    found
}

/// The bin's count chip in the timeline toolbar (`tl-bin-count`; hidden at
/// 0).
fn bin_count(nested: &mut Nested) -> u64 {
    let gui = nested.window(0);
    gui.wait_idle();
    gui.labels("tl-bin-count")
        .first()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// A logical rect in the shot's device px (`opts.scale`).
fn device(nested: &Nested, [x, y, w, h]: [f64; 4]) -> [f64; 4] {
    let s = nested.opts.scale;
    [x * s, y * s, w * s, h * s]
}

fn device_px(nested: &Nested, rect: [f64; 4]) -> [u32; 4] {
    device(nested, rect).map(|v| v.round().max(0.0) as u32)
}

/// A `--json` CLI run's stdout lines, as JSON.
fn json_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
        .collect()
}

/// The video stream of `file` (ffprobe).
fn video_stream(file: &Path) -> Value {
    let probe = ffprobe(file);
    probe["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["codec_type"] == "video"))
        .cloned()
        .unwrap_or_else(|| panic!("no video stream in {}: {probe}", file.display()))
}

/// The main-loop probe's report (the test channel's `probe` op).
fn probe(nested: &mut Nested) -> Value {
    let report = nested.window(0).ctl(json!({"op": "probe"}));
    assert_eq!(report["on"], true, "the probe runs: {report}");
    report
}

/// The time a step took, for the measurements file.
struct Steps {
    started: Instant,
    times: Vec<(String, u128)>,
}

impl Steps {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            times: Vec::new(),
        }
    }

    fn done(&mut self, step: &str) {
        let ms = self.started.elapsed().as_millis();
        self.times.push((step.to_owned(), ms));
        self.started = Instant::now();
    }
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn smoke_steps_1_to_10() {
    let mut nested = Nested::start(
        AREA,
        "smoke",
        Options::default()
            .exclusive("the main-loop probe (G§8.6): a stall over 50 ms fails it under gles2"),
    );
    let project = nested.env.empty_dir("smoke");
    design_fixture(&project, DesignVariant::Smoke);
    let mut steps = Steps::new();
    let mut launch = Launch::a11y().with_env("QREC_GUI_PROBE", "1");
    // `QREC_SMOKE_BIN`: the steps on another window binary, as the
    // package's optimized one (`result/bin/qrec`): the probe's stalls are
    // the build's the window runs.
    if let Some(bin) = std::env::var_os("QREC_SMOKE_BIN") {
        launch.bin = Some(PathBuf::from(bin));
    }
    let gles2 = nested.opts.renderer == Renderer::Gles2;
    if gles2 {
        // GSK names each GL program it compiles (with its source): none may
        // compile once the window settled (`gpu_warmup`, G§8.6).
        launch = launch.with_env("GSK_DEBUG", "shaders");
    }
    let pid = nested.launch(&project, launch).pid();

    // 1. The GUI host answers, with the window's pid.
    let status1 = wait_for_gui_host(&nested, &project, Duration::from_secs(60));
    assert_eq!(status1["data"]["runtime"]["host"], "gui", "{status1}");
    assert_eq!(
        status1["data"]["pid"].as_u64(),
        Some(u64::from(pid)),
        "{status1}"
    );
    let gui = nested.window(0);
    gui.wait_phase("open", Duration::from_secs(30));
    gui.wait_idle();
    // The window settled: the stalls so far were the project opening, and so
    // were the GL programs compiled so far.
    let opened = probe(&mut nested);
    let compiled_opening = compiled_programs(&nested.window(0).output());
    steps.done("1 status");

    // 2. A split through the GUI host: a seq, one more clip, a screenshot.
    let before = clip_count(&nested, &project);
    let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "45f"]);
    assert_eq!(split["ok"], true, "{split}");
    assert!(split["data"]["seq"].as_u64().is_some(), "{split}");
    assert_eq!(clip_count(&nested, &project), before + 1);
    nested.window(0).wait_idle();
    nested.screenshot("split");
    steps.done("2 split");

    // 3. Undo, redo, undo.
    for step in ["undo", "redo", "undo"] {
        let reply = nested.cli_json(&project, &[step]);
        assert_eq!(reply["ok"], true, "{step}: {reply}");
    }
    assert_eq!(clip_count(&nested, &project), before);
    steps.done("3 undo redo undo");

    // 4. A take recorded through the GUI host (routed: no headless host
    // spawns), REC in the shot, then a placed take: one more clip.
    let bin_before = bin_count(&mut nested);
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "record start: {start}");
    assert_eq!(start["data"]["result"]["take"], "take-0010", "{start}");
    let recording = status(&nested, &project);
    assert_eq!(
        recording["runtime"]["record"]["phase"], "recording",
        "{recording}"
    );
    assert_eq!(recording["runtime"]["host"], "gui", "{recording}");
    assert_eq!(recording["pid"].as_u64(), Some(u64::from(pid)));
    assert!(
        headless_hosts(&nested).is_empty(),
        "routed to the window, nothing spawned: {:?}",
        headless_hosts(&nested)
    );
    nested.window(0).wait_idle();
    let shot = nested.screenshot("recording");
    // REC: the `record` line along the header's bottom edge, just above
    // its kept hairline (S04: `record` at y 46, `border` at 47), and Stop
    // in it.
    // Each 1 px row as the device row under its center (at 1.5 the rows
    // spread over two device px).
    let row = |y: f64| {
        let [x, _, w, _] = device_px(&nested, [200.0, y, 1200.0, 1.0]);
        [x, nested.opts.pixel_px(200.0, y).1, w, 1]
    };
    let line = row(46.0);
    let red = count_near(&shot, line, token_rgb("swamp-night", "record"), 2);
    let hairline = row(47.0);
    let border = count_near(&shot, hairline, token_rgb("swamp-night", "border"), 2);
    assert_eq!(
        (red, border),
        (
            (line[2] * line[3]) as usize,
            (hairline[2] * hairline[3]) as usize
        ),
        "the record line over the header's hairline: {}",
        shot.display()
    );
    let header = nested.window(0).labels("region-header").join(" ");
    assert!(header.contains("Stop"), "{header:?}");
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "the a11y walk while recording: {a11y}");
    // At least MIN_TAKE_FRAMES (10) recorded before the stop.
    thread::sleep(Duration::from_millis(1000));
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "record stop: {stop}");
    let landed = &stop["data"]["result"];
    assert_eq!(landed["take"], "take-0010", "{stop}");
    assert_eq!(landed["status"], "ok", "{stop}");
    assert_eq!(landed["placed"], true, "a placed TakeFinalizedView: {stop}");
    assert!(landed["durationFrames"].as_u64() >= Some(10), "{stop}");
    let new_clip = landed["clips"][0]
        .as_str()
        .unwrap_or_else(|| panic!("the placed clip: {stop}"))
        .to_owned();
    let ids = clip_ids(&nested, &project);
    assert_eq!(ids.len(), before + 1, "{ids:?}");
    assert!(ids.contains(&new_clip), "{new_clip} in {ids:?}");
    assert_eq!(
        bin_count(&mut nested),
        bin_before,
        "a placed take isn't in the bin"
    );
    steps.done("4 record start and stop");

    // 5. Discard the last take: its clip goes, the bin has one more.
    let discard = nested.cli_json(&project, &["take", "discard-last"]);
    assert_eq!(discard["ok"], true, "take discard-last: {discard}");
    let ids = clip_ids(&nested, &project);
    assert_eq!(ids.len(), before, "{ids:?}");
    assert!(!ids.contains(&new_clip), "{new_clip} is gone: {ids:?}");
    assert_eq!(bin_count(&mut nested), bin_before + 1, "take 10 in the bin");
    steps.done("5 discard-last");

    // 6. Seek, play a second, pause: the playhead is past 10, in Program.
    for args in [&["seek", "--to", "10f"][..], &["play"]] {
        let reply = nested.cli_json(&project, args);
        assert_eq!(reply["ok"], true, "{args:?}: {reply}");
    }
    thread::sleep(Duration::from_secs(1));
    let pause = nested.cli_json(&project, &["pause"]);
    assert_eq!(pause["ok"], true, "pause: {pause}");
    // The window parks the playhead at the paused player's frame (its own
    // seek, answered once it is idle).
    nested.window(0).wait_idle();
    let local = nested.window(0).local();
    assert_eq!(local["programPlaying"], false, "{local}");
    let player_ns = local["playerNs"]
        .as_u64()
        .unwrap_or_else(|| panic!("a player: {local}"));
    let player_frame = player_ns * 30 / 1_000_000_000;
    let ui = status(&nested, &project)["ui"].clone();
    let playhead = ui["playhead"].as_u64().unwrap_or_default();
    assert!(playhead > 10, "the playhead went past 10: {ui}");
    assert!(
        playhead.abs_diff(player_frame) <= 1,
        "the playhead {playhead} is where the player paused ({player_ns} ns: frame {player_frame})"
    );
    assert_eq!(ui["canvasMode"], "program", "{ui}");
    let paused = json!({"playhead": playhead, "playerNs": player_ns});
    let shot = nested.screenshot("program");
    assert_live_picture_drawn_at(&shot, device(&nested, [128.0, 64.0, 960.0, 540.0]));
    steps.done("6 seek play pause");

    // 7. R from the keyboard starts a take and R stops it: the keys reach
    // the window; the take lands.
    nested.keys(&["-k", "r"]);
    wait_phase(&nested, &project, "recording", Duration::from_secs(20));
    thread::sleep(Duration::from_millis(1000));
    nested.keys(&["-k", "r"]);
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    let ids = clip_ids(&nested, &project);
    assert_eq!(ids.len(), before + 1, "the keyed take landed: {ids:?}");
    steps.done("7 wtype r, r");

    // 8. A draft export through the GUI host: progress lines, then done; the
    // file is the canvas's size and rate.
    let export = nested.cli(&project, &["export", "--quality", "draft", "--json"]);
    let lines = json_lines(&export.stdout);
    assert!(
        export.status.success(),
        "export: {lines:?}\n{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let (done, progress) = lines.split_last().expect("the export's lines");
    assert_eq!(done["ok"], true, "{done}");
    assert!(!progress.is_empty(), "progress lines before the reply");
    for line in progress {
        assert_eq!(line["event"], "progress", "{line}");
        assert!(line["total"].as_u64() > Some(0), "{line}");
    }
    let file = PathBuf::from(done["data"]["result"]["path"].as_str().unwrap_or_default());
    assert!(file.is_file(), "the export {}", file.display());
    let video = video_stream(&file);
    assert_eq!(
        (video["width"].as_u64(), video["height"].as_u64()),
        (Some(CANVAS.0), Some(CANVAS.1)),
        "{video}"
    );
    assert_eq!(video["r_frame_rate"], FPS, "{video}");
    let frames = done["data"]["result"]["frames"]
        .as_u64()
        .unwrap_or_default();
    assert_eq!(
        video["nb_read_packets"]
            .as_str()
            .and_then(|n| n.parse().ok()),
        Some(frames),
        "every frame exported: {video}"
    );
    steps.done("8 export");

    // 9. A frame saved at 0: a PNG the canvas's size.
    let saved = nested.cli_json(&project, &["frame", "save", "--at", "0f"]);
    assert_eq!(saved["ok"], true, "frame save: {saved}");
    let png = PathBuf::from(saved["data"]["result"]["path"].as_str().unwrap_or_default());
    assert!(png.starts_with(project.join("frames")), "{}", png.display());
    let still = video_stream(&png);
    assert_eq!(still["codec_name"], "png", "{still}");
    assert_eq!(
        (still["width"].as_u64(), still["height"].as_u64()),
        (Some(CANVAS.0), Some(CANVAS.1)),
        "{still}"
    );
    steps.done("9 frame save");

    // Throughout: the a11y walk is empty, nothing panicked.
    let gui = nested.window(0);
    gui.wait_idle();
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
    assert_eq!(gui.local()["panics"], 0);

    // The probe: what stalled while the steps ran.
    let end = probe(&mut nested);
    let settled_ms = opened["runningMs"].as_f64().unwrap_or_default();
    let stalls: Vec<&Value> = end["kept"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["atMs"].as_f64() > Some(settled_ms))
        .collect();
    let during =
        end["stalls"].as_u64().unwrap_or_default() - opened["stalls"].as_u64().unwrap_or_default();
    let measurements = json!({
        "renderer": format!("{:?}", nested.opts.renderer),
        "scale": nested.opts.scale,
        "steps": steps.times.iter().map(|(s, ms)| json!({"step": s, "ms": ms})).collect::<Vec<_>>(),
        "paused": paused,
        "probe": {"opened": opened, "end": end, "stallsDuringSteps": during},
    });
    let path = nested.shots.join("smoke-measurements.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&measurements).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    eprintln!(
        "main-loop probe: {} stalls while opening, {during} during the steps (longest {} ms \
         overall); {}",
        opened["stalls"],
        end["longestMs"],
        path.display()
    );
    for stall in &stalls {
        eprintln!("  stall: {stall}");
    }
    if gles2 {
        // No GL program compiled while the steps ran: each was a 30–70 ms
        // stall with the test's cold shader cache (the warm-up's job).
        let compiled = compiled_programs(&nested.window(0).output());
        assert!(
            !compiled_opening.is_empty(),
            "GSK_DEBUG=shaders named no program while the window opened: is its \
             output still `Gsk-Message: <time>: vertex <n>, <program>:`?"
        );
        let late = &compiled[compiled_opening.len().min(compiled.len())..];
        assert!(
            late.is_empty(),
            "GL programs compiled after the window settled (the GPU warm-up misses them, \
             G§8.6): {late:#?}"
        );
        let over: Vec<&&Value> = stalls
            .iter()
            .filter(|s| s["driftMs"].as_f64().unwrap_or_default() > GLES2_STALL_MS)
            .collect();
        assert!(
            over.is_empty(),
            "main-loop stalls over {GLES2_STALL_MS} ms under gles2 (G§8.5, G§8.6): {over:#?}"
        );
    }

    // 10. SIGTERM: 130 within 15 s, the lock free.
    let started = Instant::now();
    let gui = nested.window(0);
    gui.signal("TERM");
    let code = gui.wait_exit(Duration::from_secs(15));
    assert_eq!(
        code,
        Some(130),
        "exit after SIGTERM ({:?})",
        started.elapsed()
    );
    let after = nested.cli_json(&project, &["status"]);
    assert_eq!(
        after["data"]["host"],
        Value::Null,
        "the lock is free: {after}"
    );

    let gui = nested.window(0);
    let log = gui.gui_log();
    assert!(!log.contains("Crashed"), "the session crashed:\n{log}");
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines in the GUI log: {errors:#?}");
    assert!(
        !log.contains("panicked") && !gui.output().contains("panicked"),
        "a panic: {}",
        gui.log.display()
    );
    assert!(
        log.contains("main-loop probe: on") && log.contains(" ticks in "),
        "the probe started and summed up in the log: {}",
        gui.log.display()
    );
    gui.assert_no_criticals();
    // The probe's lines kept with the shots (`smoke-main-loop-<renderer>.txt`;
    // the report above fails the run).
    let stalls = nested.check_main_loop(0);
    eprintln!("main-loop stall lines in the log: {}", stalls.len());
}

// ------------------------------------------------------------- the package

/// The package `nix build` made: `$QREC_PACKAGE` (its out path), else the
/// repository's `result`; `None` when there is neither.
fn package() -> Option<PathBuf> {
    let dir = match std::env::var_os("QREC_PACKAGE") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            assert!(
                dir.join("bin/qrec").is_file(),
                "QREC_PACKAGE={} has no bin/qrec",
                dir.display()
            );
            dir
        }
        None => {
            let result = repo().join("result");
            if !result.join("bin/qrec").is_file() {
                return None;
            }
            result
        }
    };
    Some(
        dir.canonicalize()
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display())),
    )
}

/// What the dev shell sets that the package's wrapper must set itself: the
/// packaged window and CLI get none of them (G§6.4).
const DEV_SHELL_PATHS: [&str; 14] = [
    "XDG_DATA_DIRS",
    "GSETTINGS_SCHEMA_DIR",
    "QREC_FONT_DIR",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_REGISTRY_1_0",
    "GI_TYPELIB_PATH",
    "GIO_EXTRA_MODULES",
    "GDK_PIXBUF_MODULE_FILE",
    "LD_LIBRARY_PATH",
];

/// The files the package installs besides the binary: the desktop entry
/// (the source's, so a stale `result` fails here), the hicolor icons, the
/// fonts and their licenses.
fn assert_package_files(package: &Path) {
    let id = qrec_gui::app::APP_ID;
    let entry = package.join(format!("share/applications/{id}.desktop"));
    let source = repo().join(format!("data/{id}.desktop"));
    assert_eq!(
        fs::read_to_string(&entry).unwrap_or_default(),
        fs::read_to_string(&source).expect("the desktop entry's source"),
        "{} is {} as built (a stale result?)",
        entry.display(),
        source.display()
    );
    let icons = package.join("share/icons/hicolor");
    let svg = fs::read_to_string(icons.join(format!("scalable/apps/{id}.svg"))).unwrap_or_default();
    assert!(
        svg.contains("<svg") && !svg.contains("{{"),
        "the scalable icon has its colors: {svg}"
    );
    // The pen's app icon at every hicolor size it renders (data/install.sh).
    for size in [16, 32, 48, 64, 128, 256, 512] {
        let png = icons.join(format!("{size}x{size}/apps/{id}.png"));
        assert_eq!(png_size(&png), (size, size), "{}", png.display());
    }
    let symbolic = icons.join(format!("symbolic/apps/{id}-symbolic.svg"));
    assert!(symbolic.is_file(), "{}", symbolic.display());
    let lucide = package.join("share/licenses/qrec/LICENSE-lucide.txt");
    assert!(lucide.is_file(), "the icons' license: {}", lucide.display());
    for file in qrec_gui::theme::fonts::FILES {
        let font = package.join("share/qrec/fonts").join(file);
        assert!(font.is_file(), "{}", font.display());
    }
    let licenses = fs::read_dir(package.join("share/licenses/qrec-fonts"))
        .map(Iterator::count)
        .unwrap_or_default();
    assert_eq!(licenses, 3, "the fonts' OFL texts");
}

/// The wrapper (`bin/qrec`) sets the plugin path (the filtered
/// gst-plugins-bad, never the unfiltered one whose nvcodec and qsv hang the
/// registry scan on the dev machine; GES; gst-plugins-rs's
/// gtk4paintablesink), the GSettings schemas and the runtime tools.
fn assert_wrapper(package: &Path) {
    let wrapper = String::from_utf8_lossy(&fs::read(package.join("bin/qrec")).unwrap_or_default())
        .into_owned();
    for wanted in [
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "-gst-plugins-bad-qrec-",
        "-gst-editing-services-",
        "-gst-plugins-rs-",
        "-gst-plugins-base-",
        "-gst-plugins-good-",
        "XDG_DATA_DIRS",
        "/share/gsettings-schemas/gsettings-desktop-schemas-",
        "-adwaita-icon-theme-",
        "-hicolor-icon-theme-",
        "-ffmpeg-full-",
        "-wl-clipboard-",
        ".qrec-wrapped",
    ] {
        assert!(
            wrapper.contains(wanted),
            "the wrapper has {wanted:?}:\n{wrapper}"
        );
    }
    let unfiltered: Vec<&str> = wrapper
        .match_indices("-gst-plugins-bad-")
        .map(|(at, found)| &wrapper[at + found.len()..])
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .map(|rest| rest.split('\'').next().unwrap_or(rest))
        .collect();
    assert!(
        unfiltered.is_empty(),
        "the unfiltered gst-plugins-bad is on the wrapper's path: {unfiltered:?}"
    );
    // The package's plugins only: the system path set, not prefixed to a
    // session's, and the user's unset (WP10's code audit).
    assert!(
        !wrapper.contains("GST_PLUGIN_SYSTEM_PATH_1_0=${GST_PLUGIN_SYSTEM_PATH_1_0"),
        "the wrapper prefixes the session's plugin path:\n{wrapper}"
    );
    assert!(
        wrapper.contains("unset GST_PLUGIN_PATH_1_0"),
        "the wrapper keeps the session's user plugin path:\n{wrapper}"
    );
}

/// `<package>/bin/qrec <args> --json` as the CLI, in the env with none of
/// [`DEV_SHELL_PATHS`] and `path` as `PATH` (like the window): its reply.
fn packaged_cli(nested: &Nested, bin: &Path, path: &Path, project: &Path, args: &[&str]) -> Value {
    let mut command = nested.env.command(bin);
    for key in DEV_SHELL_PATHS {
        command.env_remove(key);
    }
    command
        .env("PATH", path)
        .env("GST_REGISTRY_UPDATE", "no")
        .arg("--project")
        .arg(project)
        .args(args)
        .arg("--json");
    let out = run_tool(&mut command, gui_support::CLI_TIMEOUT);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let last = stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("{}");
    serde_json::from_str(last)
        .unwrap_or_else(|e| panic!("{bin:?} {args:?}: {e}\n{stdout}\n{}", out.stderr))
}

/// The variables of a running process (`/proc/<pid>/environ`).
fn environ(pid: u32) -> Vec<(String, String)> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    bytes
        .split(|&b| b == 0)
        .filter_map(|var| {
            let var = String::from_utf8_lossy(var);
            let (key, value) = var.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// G§6.4 (WP10): the `nix build` result runs in nested sway on the Smoke
/// fixture with only what its wrapper gives it: no dev shell plugin,
/// schema or font paths and an empty `PATH`. The window opens (GSettings
/// schemas: libadwaita reads `org.gnome.desktop.interface`; ffmpeg is
/// found for the dependency check), its regions are BRIEF's, the bundled
/// fonts load from `<exe>/../share/qrec/fonts`, the GResource's icons and
/// art draw, the live picture shows the fake sources (GStreamer's plugin
/// path: the compositor, gtk4paintablesink) and Program plays (GES). Its
/// own CLI talks to its window. GStreamer's registry is the one Qrec picks,
/// seeded with the dev shell's and never updated (a scan loads the va
/// plugin, which opens every render node, NVIDIA's too).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype) and `nix build`'s result: G§8.4"]
fn the_packaged_build_opens_the_smoke_fixture() {
    let Some(package) = package() else {
        eprintln!(
            "skipped: no package (run `nix build` first, or set QREC_PACKAGE to its out path)"
        );
        return;
    };
    eprintln!("the package: {}", package.display());
    assert_package_files(&package);
    assert_wrapper(&package);
    let bin = package.join("bin/qrec");

    let mut nested = Nested::start(
        AREA,
        "packaged",
        Options::default()
            .exclusive("the packaged window plays and pauses the Program while the case reads it"),
    );
    let project = nested.env.empty_dir("smoke");
    design_fixture(&project, DesignVariant::Smoke);
    let no_tools = nested.env.root().join("no-tools");
    fs::create_dir_all(&no_tools).expect("an empty PATH dir");
    // The registry Qrec picks without GST_REGISTRY_1_0 (`gpu::plan_gst_env`),
    // seeded with the dev shell's (the package's plugins are the same store
    // paths), and never updated: a scan loads the va plugin, which opens
    // every render node, NVIDIA's too. The plugin path itself is checked
    // above (the wrapper) and below (the window's environment).
    let dev_registry = std::env::var_os("GST_REGISTRY_1_0")
        .map(PathBuf::from)
        .filter(|r| r.is_file())
        .expect("the dev shell's GStreamer registry (GST_REGISTRY_1_0) seeds the package's");
    let registry = nested.env.cache_home().join(format!(
        "qrec/gstreamer-registry-{}.bin",
        std::env::consts::ARCH
    ));
    fs::create_dir_all(registry.parent().expect("a parent")).expect("the registry's dir");
    fs::copy(&dev_registry, &registry).expect("seed the registry");
    let seeded = fs::read(&registry).expect("the seeded registry");

    // A session's own plugin paths (NixOS sets one for GNOME, with an
    // unfiltered gst-plugins-bad) aren't the package's: the wrapper sets
    // the system path and unsets the user's, so neither is scanned (WP10's
    // code audit: the wrapper prefixed the inherited system path).
    let session_plugins = nested.env.root().join("session/lib/gstreamer-1.0");
    fs::create_dir_all(&session_plugins).expect("a session plugin dir");
    let session_plugins = session_plugins.to_string_lossy().into_owned();
    let inherited = ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_PATH_1_0"];
    let launch = Launch {
        a11y: true,
        bin: Some(bin.clone()),
        path: Some(no_tools.clone().into()),
        unset: DEV_SHELL_PATHS
            .iter()
            .filter(|k| !inherited.contains(k))
            .map(|k| (*k).to_owned())
            .collect(),
        ..Launch::default()
    }
    .with_env("GST_PLUGIN_SYSTEM_PATH_1_0", &session_plugins)
    .with_env("GST_PLUGIN_PATH_1_0", &session_plugins)
    .with_env("GST_REGISTRY_UPDATE", "no")
    .with_env("GST_DEBUG", "GST_PLUGIN_LOADING:4")
    .with_env("GST_DEBUG_NO_COLOR", "1");
    let pid = nested.launch(&project, launch).pid();

    // Its own CLI finds its window.
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        let status = packaged_cli(&nested, &bin, &no_tools, &project, &["status"]);
        if status["data"]["host"] == "gui" {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "no packaged GUI host answered: {status}; log {}",
            nested.window(0).log.display()
        );
        thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(
        status["data"]["pid"].as_u64(),
        Some(u64::from(pid)),
        "{status}"
    );
    let gui = nested.window(0);
    gui.wait_phase("open", Duration::from_secs(30));
    gui.wait_idle();

    // The window's environment is the wrapper's.
    let vars = environ(pid);
    let var = |key: &str| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    assert_eq!(
        var("QREC_FONT_DIR"),
        None,
        "the fonts come from the package"
    );
    assert_eq!(var("GSETTINGS_SCHEMA_DIR"), None);
    let plugins = var("GST_PLUGIN_SYSTEM_PATH_1_0").unwrap_or_default();
    assert!(
        !plugins.is_empty()
            && plugins
                .split(':')
                .all(|dir| dir.starts_with("/nix/store/") && dir.ends_with("/lib/gstreamer-1.0")),
        "the wrapper's plugin path, not the session's {session_plugins}: {plugins}"
    );
    assert_eq!(
        var("GST_PLUGIN_PATH_1_0"),
        None,
        "the session's user plugin path is unset"
    );
    assert!(
        var("XDG_DATA_DIRS").is_some_and(|d| d.contains("gsettings-desktop-schemas")),
        "{:?}",
        var("XDG_DATA_DIRS")
    );

    let regions = gui.regions(&[]);
    assert_regions(&regions, &BRIEF_REGIONS, &MERGED_REGIONS, nested.opts.scale);
    let gui = nested.window(0);
    for op in ["fonts", "icons", "art"] {
        let reply = gui.ctl(json!({"op": op}));
        assert_eq!(reply["ok"], true, "{op}: {reply}");
    }
    let log = gui.gui_log();
    for file in qrec_gui::theme::fonts::FILES {
        let line = format!("font {}/bin/../share/qrec/fonts/{file}", package.display());
        assert!(log.contains(&line), "{line:?} in the GUI log");
    }
    assert!(!log.contains("can't load the font"), "{log}");
    let shot = nested.screenshot("live");
    assert_live_picture_drawn_at(&shot, device(&nested, [128.0, 64.0, 960.0, 540.0]));

    // Program plays (GES): the player moves, then parks. The pause waits
    // until the program plays past its first frame: the park snaps the
    // paused player to the frame the picture shows (G§3.10 step 2), and
    // the packaged window can spend most of a second loading the program,
    // so a pause 500 ms after `play` parked it at 0 (1 run of 4 on pixman
    // after a new `nix build` at the play lane's integration, 2 of 4 on a
    // warm package at its audit).
    let play = packaged_cli(&nested, &bin, &no_tools, &project, &["play"]);
    assert_eq!(play["ok"], true, "play: {play}");
    let frame_ns = 1_000_000_000 / 30;
    gui_support::poll(
        Duration::from_secs(20),
        "the program to play past its first frame",
        || {
            let local = nested.window(0).local();
            (local["programPlaying"] == true && local["playerNs"].as_u64() > Some(frame_ns))
                .then_some(())
        },
    );
    let pause = packaged_cli(&nested, &bin, &no_tools, &project, &["pause"]);
    assert_eq!(pause["ok"], true, "pause: {pause}");
    let gui = nested.window(0);
    gui.wait_idle();
    let local = gui.local();
    assert!(
        local["playerNs"].as_u64() > Some(0),
        "the player moved: {local}"
    );
    assert_eq!(local["programPlaying"], false, "{local}");
    let shot = nested.screenshot("program");
    assert_live_picture_drawn_at(&shot, device(&nested, [128.0, 64.0, 960.0, 540.0]));

    let gui = nested.window(0);
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
    assert_eq!(gui.local()["panics"], 0);
    gui.assert_no_criticals();
    let log = gui.gui_log();
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines in the GUI log: {errors:#?}");
    assert!(
        log.contains("player: Loaded"),
        "the Program player loaded (GES)"
    );
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(Duration::from_secs(20)), Some(0));
    // Nothing scanned the plugins: the registry is as seeded and the va
    // plugin never loaded (GST_PLUGIN_LOADING logs every plugin loaded).
    let output = nested.window(0).output();
    assert!(
        output.contains("GST_PLUGIN_LOADING"),
        "GStreamer's plugin loading is logged"
    );
    assert!(
        !output.contains("libgstva.so\" loaded"),
        "the va plugin loaded: {}",
        nested.window(0).log.display()
    );
    assert!(
        fs::read(&registry).ok() == Some(seeded),
        "the registry was rewritten"
    );
}
