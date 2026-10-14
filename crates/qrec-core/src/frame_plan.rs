//! The frame-save planner (A§9.4). Owner: output.
//!
//! A saved frame goes through the export graph as a one-frame unit (`n = 1`,
//! no audio), so it matches the export, then becomes an sRGB PNG. With a
//! layer, it is that layer's raw source frame at native resolution, picked
//! with the same frame selection.

use std::collections::BTreeSet;

use crate::error::CoreError;
use crate::export_plan::{layer_inputs, select_frames, video_graph};
use crate::ids::{Frame, LayerId, TakeId};
use crate::media_argv::{TO_SRGB_PNG, ffmpeg_prelude, file_arg, png_output_args, seconds_arg};
use crate::model::{Project, StreamKind};
use crate::plans::FramePlan;
use crate::time::timecode;
use crate::timeline::{source_at, total_frames};

/// The take and take frame under timeline frame `at`. `Invalid` when the
/// timeline is empty or `at` is past its end.
pub fn frame_source(project: &Project, at: Frame) -> Result<(TakeId, Frame), CoreError> {
    let clips = &project.timeline.clips;
    let total = total_frames(clips);
    if total == 0 {
        return Err(CoreError::invalid("nothing to save: the timeline is empty"));
    }
    let (take, frame) = source_at(clips, at).ok_or_else(|| {
        CoreError::invalid(format!(
            "{} is past the end of the timeline ({})",
            timecode(at, project.canvas.fps),
            timecode(total, project.canvas.fps)
        ))
        .with_hint(format!("pick a frame from 0f to {}f", total - 1))
    })?;
    Ok((take.clone(), frame))
}

/// The PNG of timeline frame `at` through the export graph as a one-frame
/// unit (sRGB). With `layer`: that layer's raw source frame at native
/// resolution. `out` is the output path (absolute, or project-relative for
/// auto names).
pub fn plan_frame(
    project: &Project,
    at: Frame,
    layer: Option<&LayerId>,
    out: &str,
) -> Result<FramePlan, CoreError> {
    let (take_id, take_frame) = frame_source(project, at)?;
    let take = project.take(&take_id).ok_or_else(|| {
        CoreError::invalid(format!(
            "the clip at {at}f uses {take_id}, which isn't in the project"
        ))
    })?;
    let canvas = &project.canvas;
    let mut argv = ffmpeg_prelude(true);
    let mut files = BTreeSet::new();
    match layer {
        None => {
            let layers = layer_inputs(take, canvas, take_frame, 1);
            let video = video_graph(canvas, &layers);
            let mut chains = video.chains;
            chains.push(format!(
                "[{}]trim=end_frame=1,setsar=1,format=yuv420p,{TO_SRGB_PNG}[v]",
                video.label
            ));
            argv.extend(video.inputs);
            argv.extend(["-filter_complex".to_owned(), chains.join(";")]);
            files.extend(video.files);
        }
        Some(id) => {
            let layer = take.scene.layer(id).ok_or_else(|| {
                CoreError::not_found(format!("layer {id} isn't in {take_id}"))
                    .with_hint(format!("see the layers with `qrec take show {take_id}`"))
            })?;
            if layer.missing {
                return Err(CoreError::invalid(format!(
                    "layer {id} has no picture in {take_id}: its source was lost"
                )));
            }
            let stream = take
                .stream(id.as_str())
                .filter(|s| s.kind == StreamKind::Video && s.width.is_some() && s.height.is_some())
                .ok_or_else(|| {
                    CoreError::invalid(format!("layer {id} has no recorded video in {take_id}"))
                })?;
            let selection = select_frames(stream, canvas.fps, take_frame);
            if selection.lead > 0 {
                return Err(CoreError::invalid(format!(
                    "layer {id} has no picture at {}: its recording starts later",
                    timecode(at, canvas.fps)
                )));
            }
            if let Some(seek) = selection.seek_ns(stream.fps.unwrap_or(canvas.fps)) {
                argv.extend(["-ss".to_owned(), seconds_arg(seek)]);
            }
            argv.extend([
                "-i".to_owned(),
                file_arg(&stream.file),
                "-filter_complex".to_owned(),
                format!("[0:v]{TO_SRGB_PNG}[v]"),
            ]);
            files.insert(stream.file.clone());
        }
    }
    argv.extend(["-map", "[v]", "-frames:v", "1"].map(String::from));
    argv.extend(png_output_args(out));
    Ok(FramePlan {
        out: out.to_owned(),
        tmp_ext: "png".to_owned(),
        argv,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use crate::export_plan::tests::render_argv;
    use crate::model::Fps;
    use crate::testkit::ProjectBuilder;

    fn project() -> Project {
        ProjectBuilder::new()
            .name("my-video")
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "mic")
            .take("take-0001", 300)
            .take("take-0002", 300)
            .with_take("take-0002", |take| {
                take.streams[1].start_offset_ns = 41_000_000;
                take.scene.layers[1].opacity = 0.75;
            })
            .clip("c1", "take-0001", 0, 100)
            .clip("c2", "take-0002", 0, 50)
            .build()
    }

    #[test]
    fn sources() {
        let p = project();
        assert_eq!(frame_source(&p, 0).unwrap(), (TakeId::numbered(1), 0));
        assert_eq!(frame_source(&p, 99).unwrap(), (TakeId::numbered(1), 99));
        assert_eq!(frame_source(&p, 100).unwrap(), (TakeId::numbered(2), 0));
        let err = frame_source(&p, 150).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "00:00:05:00 is past the end of the timeline (00:00:05:00)"
        );
        assert_eq!(err.hint.as_deref(), Some("pick a frame from 0f to 149f"));
        let empty = ProjectBuilder::new().build();
        assert_eq!(
            frame_source(&empty, 0).unwrap_err().message,
            "nothing to save: the timeline is empty"
        );
    }

    #[test]
    fn composite_frames() {
        let p = project();
        let plan = plan_frame(&p, 137, None, "frames/my-video-take-0002-00000407.png").unwrap();
        assert_eq!(plan.tmp_ext, "png");
        assert_eq!(
            plan.argv.last().unwrap(),
            "file:frames/my-video-take-0002-00000407.png"
        );
        assert_eq!(plan.files.len(), 2);
        insta::assert_snapshot!("frame_composite", render_argv(&plan.argv));
        // Frame 100 is the second take's first frame: its camera starts one
        // frame late, so only the screen is drawn.
        let plan = plan_frame(&p, 100, None, "/tmp/x.png").unwrap();
        assert_eq!(plan.files.len(), 1);
        assert!(
            !plan.argv.iter().any(|a| a == "-ss"),
            "frame 0 needs no seek"
        );
    }

    #[test]
    fn raw_layer_frames() {
        let p = project();
        let plan = plan_frame(&p, 137, Some(&"L2".parse().unwrap()), "/home/me/cam.png").unwrap();
        insta::assert_snapshot!("frame_raw_layer", render_argv(&plan.argv));
        // The camera's frame 36 (take frame 37 minus one frame of offset).
        assert!(
            plan.argv
                .windows(2)
                .any(|w| w[0] == "-ss" && w[1] == "1.198000")
        );
        let err = plan_frame(&p, 100, Some(&"L2".parse().unwrap()), "/tmp/x.png").unwrap_err();
        assert_eq!(
            err.message,
            "layer L2 has no picture at 00:00:03:10: its recording starts later"
        );
        let err = plan_frame(&p, 0, Some(&"L9".parse().unwrap()), "/tmp/x.png").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let mut lost = p.clone();
        lost.takes
            .get_mut(&TakeId::numbered(1))
            .unwrap()
            .scene
            .layers[1]
            .missing = true;
        let err = plan_frame(&lost, 0, Some(&"L2".parse().unwrap()), "/tmp/x.png").unwrap_err();
        assert!(err.message.contains("source was lost"), "{}", err.message);
        let mut no_media = p.clone();
        no_media
            .takes
            .get_mut(&TakeId::numbered(1))
            .unwrap()
            .streams
            .retain(|s| s.id != "L2");
        let err = plan_frame(&no_media, 0, Some(&"L2".parse().unwrap()), "/tmp/x.png").unwrap_err();
        assert_eq!(err.message, "layer L2 has no recorded video in take-0001");
    }

    #[test]
    fn other_recorded_rates() {
        let p = ProjectBuilder::new()
            .canvas(1280, 720, Fps::new(30, 1))
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .with_take("take-0001", |take| {
                take.streams[0].fps = Some(Fps::new(60, 1))
            })
            .clip("c1", "take-0001", 0, 100)
            .build();
        let plan = plan_frame(&p, 10, Some(&"L1".parse().unwrap()), "/tmp/x.png").unwrap();
        // Canvas frame 10 = recorded frame 20 = 333.333 ms, minus 2 ms.
        assert!(
            plan.argv
                .windows(2)
                .any(|w| w[0] == "-ss" && w[1] == "0.331333")
        );
    }
}
