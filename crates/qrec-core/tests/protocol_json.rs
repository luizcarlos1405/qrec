//! Every protocol type round-trips through JSON (A§4.4), and the JSON forms
//! people and agents see are pinned with snapshots.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;
use qrec_core::history::HistoryEntry;
use qrec_core::layout::{Placement, SourceRegion};
use qrec_core::plans::*;
use qrec_core::protocol::*;
use qrec_core::state::*;
use qrec_core::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text =
        serde_json::to_string(value).unwrap_or_else(|e| panic!("{value:?} doesn't serialize: {e}"));
    let back: T =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{text} doesn't parse back: {e}"));
    assert_eq!(&back, value, "{text}");
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

fn clip(id: &str) -> ClipId {
    id.parse().unwrap()
}

fn take(n: u32) -> TakeId {
    TakeId::numbered(n)
}

fn t(s: &str) -> TimeArg {
    s.parse().unwrap()
}

fn ts() -> DateTime<Utc> {
    "2026-10-14T12:00:00Z".parse().unwrap()
}

/// One value per `Command` variant (checked by `every_variant_is_covered`).
fn all_commands() -> Vec<Command> {
    vec![
        Command::Init {
            canvas: Some("1920x1080@30".parse().unwrap()),
            name: Some("demo".into()),
        },
        Command::Init {
            canvas: None,
            name: None,
        },
        Command::ProjectSet {
            name: Some("Talk".into()),
            canvas: Some("@29.97".parse().unwrap()),
            background: Some("#12170f".parse().unwrap()),
            countdown_secs: Some(3),
            encoder: Some(EncoderPref::X264),
            vaapi_device: Nullable::Set("/dev/dri/renderD129".into()),
        },
        Command::ProjectSet {
            name: None,
            canvas: None,
            background: None,
            countdown_secs: None,
            encoder: None,
            vaapi_device: Nullable::Clear,
        },
        Command::ProjectSet {
            name: None,
            canvas: Some("1280x720".parse().unwrap()),
            background: None,
            countdown_secs: None,
            encoder: None,
            vaapi_device: Nullable::Keep,
        },
        Command::LayerAdd {
            target: SceneTarget::Project,
            source: VideoSourceArg::Screen {
                output: "DP-4".into(),
            },
            name: Some("Main".into()),
            rect: Some(Rect::new(-10, 0, 1920, 1080)),
        },
        Command::LayerAdd {
            target: SceneTarget::Project,
            source: VideoSourceArg::Camera {
                device: "/dev/video0".into(),
            },
            name: None,
            rect: None,
        },
        Command::LayerSet {
            target: SceneTarget::Take(take(1)),
            layer: LayerId::numbered(2),
            patch: LayerPatch {
                name: Some("Face".into()),
                visible: Some(false),
                opacity: Some(0.8),
                rect: Some(Rect::new(1500, 760, 384, 288)),
                crop: Some(Crop {
                    top: 1,
                    right: 2,
                    bottom: 3,
                    left: 4,
                }),
                fit: Some(Fit::Cover),
                locked: Some(true),
            },
        },
        Command::LayerOrder {
            target: SceneTarget::Project,
            layer: LayerId::numbered(1),
            to: 2,
        },
        Command::LayerRemove {
            target: SceneTarget::Take(take(12)),
            layer: LayerId::numbered(1),
        },
        Command::AudioAdd {
            target: SceneTarget::Project,
            role: AudioRole::Mic,
            source: None,
            label: None,
        },
        Command::AudioAdd {
            target: SceneTarget::Project,
            role: AudioRole::System,
            source: Some("alsa_output.pci.analog-stereo.monitor".into()),
            label: Some("Desktop".into()),
        },
        Command::AudioSet {
            target: SceneTarget::Project,
            track: TrackId::numbered(1),
            patch: AudioPatch {
                label: Some("Yeti".into()),
                source: Some("alsa_input.usb".into()),
                gain_db: Some(-3.5),
                muted: Some(true),
                sync_offset_ms: Some(-80),
            },
        },
        Command::AudioRemove {
            target: SceneTarget::Project,
            track: TrackId::numbered(2),
        },
        Command::SceneApply {
            from: SceneTarget::Project,
            to: SceneDest::AllTakes,
        },
        Command::SceneApply {
            from: SceneTarget::Take(take(3)),
            to: SceneDest::Takes(vec![take(1), take(2)]),
        },
        Command::SceneApply {
            from: SceneTarget::Take(take(3)),
            to: SceneDest::Project,
        },
        Command::AutocutSettings {
            patch: AutocutPatch {
                auto_run_after_take: Some(true),
                track: Nullable::Set(TrackId::numbered(1)),
                threshold_db: Some(-40.0),
                min_silence_ms: Some(500),
                padding_ms: Some(100),
                min_keep_ms: Some(250),
            },
        },
        Command::AutocutSettings {
            patch: AutocutPatch {
                track: Nullable::Clear,
                ..Default::default()
            },
        },
        Command::RecordStart {
            mode: RecordModeArg::Append,
            countdown: Some(3),
            wait: true,
        },
        Command::RecordStart {
            mode: RecordModeArg::Over(vec![clip("c7f3"), clip("c8a1")]),
            countdown: None,
            wait: false,
        },
        Command::RecordStart {
            mode: RecordModeArg::OverSelection,
            countdown: None,
            wait: false,
        },
        Command::RecordStop,
        Command::RecordCancel,
        Command::RecordToggle { wait: true },
        Command::TakeDiscardLast,
        Command::TakePurge {
            takes: vec![take(1), take(3)],
        },
        Command::ClipAdd {
            take: take(2),
            at: InsertAt::End,
        },
        Command::ClipAdd {
            take: take(2),
            at: InsertAt::Index(0),
        },
        Command::ClipAdd {
            take: take(2),
            at: InsertAt::After(clip("c7f3")),
        },
        Command::ClipAdd {
            take: take(2),
            at: InsertAt::AfterSelection,
        },
        Command::ClipMove {
            clips: vec![clip("c7f3")],
            to: MoveTo::By(-1),
        },
        Command::ClipMove {
            clips: vec![clip("c7f3"), clip("c8a1")],
            to: MoveTo::Index(4),
        },
        Command::ClipMove {
            clips: vec![clip("c7f3")],
            to: MoveTo::Before(clip("c001")),
        },
        Command::ClipMove {
            clips: vec![clip("c7f3")],
            to: MoveTo::After(clip("c001")),
        },
        Command::ClipTrim {
            clip: clip("c7f3"),
            start: Some(t("+1f")),
            end: Some(t("00:00:13:21")),
            timeline: false,
        },
        Command::ClipTrim {
            clip: clip("c7f3"),
            start: None,
            end: Some(t("13.7s")),
            timeline: true,
        },
        Command::ClipSplit {
            clip: Some(clip("c7f3")),
            at: t("13f"),
            timeline: false,
        },
        Command::ClipSplit {
            clip: None,
            at: t("00:00:13.700"),
            timeline: true,
        },
        Command::ClipRemove {
            clips: vec![clip("c7f3"), clip("c8a1")],
        },
        Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides {
                track: Some(TrackId::numbered(1)),
                threshold_db: Some(-35.0),
                min_silence_ms: Some(400),
                padding_ms: Some(120),
                min_keep_ms: Some(200),
            },
            dry_run: true,
        },
        Command::Autocut {
            scope: ClipScope::Clips(vec![clip("c7f3")]),
            overrides: Default::default(),
            dry_run: false,
        },
        Command::Undo { steps: 1 },
        Command::Redo { steps: 3 },
        Command::HistoryRestore { seq: 12 },
        Command::HistoryClear,
        Command::Export {
            format: ExportFormat::Webm,
            quality: Quality::High,
            codec: Some(VideoCodec::Av1),
            out: Some("/home/me/out.webm".into()),
        },
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Draft,
            codec: None,
            out: None,
        },
        Command::ExportCancel {
            job: Some(JobId(4)),
        },
        Command::ExportCancel { job: None },
        Command::FrameSave {
            at: t("412f"),
            layer: Some(LayerId::numbered(2)),
            out: Some("/tmp/f.png".into()),
        },
        Command::FrameSave {
            at: t("1s"),
            layer: None,
            out: None,
        },
        Command::Select {
            op: SelectOp::Set(vec![clip("c7f3")]),
        },
        Command::Select {
            op: SelectOp::Toggle(clip("c7f3")),
        },
        Command::Select {
            op: SelectOp::RangeTo(clip("c7f3")),
        },
        Command::Select { op: SelectOp::Prev },
        Command::Select { op: SelectOp::Next },
        Command::Select { op: SelectOp::All },
        Command::Select {
            op: SelectOp::Clear,
        },
        Command::Seek {
            to: SeekTarget::To(t("00:00:13:21")),
        },
        Command::Seek {
            to: SeekTarget::By(t("-1s")),
        },
        Command::Seek {
            to: SeekTarget::EditPrev,
        },
        Command::Seek {
            to: SeekTarget::EditNext,
        },
        Command::Seek {
            to: SeekTarget::Start,
        },
        Command::Seek {
            to: SeekTarget::End,
        },
        Command::Seek {
            to: SeekTarget::Player {
                frame: 2467,
                rev: 3,
            },
        },
        Command::Play,
        Command::Pause,
        Command::Shuttle {
            dir: ShuttleDir::Reverse,
        },
        Command::Shuttle {
            dir: ShuttleDir::Stop,
        },
        Command::Shuttle {
            dir: ShuttleDir::Forward,
        },
        Command::SetCanvasMode {
            mode: CanvasMode::Program,
        },
        Command::SetUi {
            patch: UiPatch {
                zoom: Nullable::Set(2.5),
                scroll_px: Some(120.0),
                bin_open: Some(true),
                snapping: Some(false),
                solo: Nullable::Set(TrackId::numbered(1)),
                lane_mute: Some(BTreeSet::from([TrackId::numbered(2), TrackId::numbered(1)])),
                selected_layer: Nullable::Clear,
                panels: Some(BTreeMap::from([
                    ("bin".to_owned(), 280.0),
                    ("layers".to_owned(), 320.5),
                ])),
            },
        },
        Command::SetUi {
            patch: UiPatch {
                zoom: Nullable::Clear,
                solo: Nullable::Clear,
                ..Default::default()
            },
        },
    ]
}

/// Exhaustive on purpose: a new variant breaks the build until it gets a
/// round-trip case above.
fn variant(cmd: &Command) -> usize {
    match cmd {
        Command::Init { .. } => 0,
        Command::ProjectSet { .. } => 1,
        Command::LayerAdd { .. } => 2,
        Command::LayerSet { .. } => 3,
        Command::LayerOrder { .. } => 4,
        Command::LayerRemove { .. } => 5,
        Command::AudioAdd { .. } => 6,
        Command::AudioSet { .. } => 7,
        Command::AudioRemove { .. } => 8,
        Command::SceneApply { .. } => 9,
        Command::AutocutSettings { .. } => 10,
        Command::RecordStart { .. } => 11,
        Command::RecordStop => 12,
        Command::RecordCancel => 13,
        Command::RecordToggle { .. } => 14,
        Command::TakeDiscardLast => 15,
        Command::TakePurge { .. } => 16,
        Command::ClipAdd { .. } => 17,
        Command::ClipMove { .. } => 18,
        Command::ClipTrim { .. } => 19,
        Command::ClipSplit { .. } => 20,
        Command::ClipRemove { .. } => 21,
        Command::Autocut { .. } => 22,
        Command::Undo { .. } => 23,
        Command::Redo { .. } => 24,
        Command::HistoryRestore { .. } => 25,
        Command::HistoryClear => 26,
        Command::Export { .. } => 27,
        Command::ExportCancel { .. } => 28,
        Command::FrameSave { .. } => 29,
        Command::Select { .. } => 30,
        Command::Seek { .. } => 31,
        Command::Play => 32,
        Command::Pause => 33,
        Command::Shuttle { .. } => 34,
        Command::SetCanvasMode { .. } => 35,
        Command::SetUi { .. } => 36,
    }
}
const VARIANTS: usize = 37;

#[test]
fn every_variant_is_covered() {
    let covered: BTreeSet<usize> = all_commands().iter().map(variant).collect();
    assert_eq!(covered, (0..VARIANTS).collect());
}

#[test]
fn every_command_round_trips() {
    for cmd in all_commands() {
        round_trip(&cmd);
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(value["cmd"], cmd.name(), "name() matches the tag");
        assert_eq!(cmd.to_json(), value);
    }
}

#[test]
fn command_json_forms() {
    let move_left = Command::ClipMove {
        clips: vec![clip("c7f3")],
        to: MoveTo::By(-1),
    };
    insta::assert_snapshot!(json(&move_left), @r#"{"cmd":"clipMove","clips":["c7f3"],"to":{"by":-1}}"#);

    let on_take = Command::LayerSet {
        target: SceneTarget::Take(take(1)),
        layer: LayerId::numbered(1),
        patch: LayerPatch {
            opacity: Some(0.5),
            ..Default::default()
        },
    };
    insta::assert_snapshot!(json(&on_take), @r#"{"cmd":"layerSet","target":"take-0001","layer":"L1","patch":{"opacity":0.5}}"#);

    let clear_track = Command::AutocutSettings {
        patch: AutocutPatch {
            track: Nullable::Clear,
            ..Default::default()
        },
    };
    insta::assert_snapshot!(json(&clear_track), @r#"{"cmd":"autocutSettings","patch":{"track":null}}"#);

    let fit = Command::SetUi {
        patch: UiPatch {
            zoom: Nullable::Clear,
            scroll_px: Some(0.0),
            ..Default::default()
        },
    };
    insta::assert_snapshot!(json(&fit), @r#"{"cmd":"setUi","patch":{"zoom":null,"scrollPx":0.0}}"#);

    let split = Command::ClipSplit {
        clip: Some(clip("c7f3")),
        at: t("13f"),
        timeline: false,
    };
    insta::assert_snapshot!(json(&split), @r#"{"cmd":"clipSplit","clip":"c7f3","at":"13f","timeline":false}"#);

    let init = Command::Init {
        canvas: Some("1920x1080@30000/1001".parse().unwrap()),
        name: None,
    };
    insta::assert_snapshot!(json(&init), @r#"{"cmd":"init","canvas":"1920x1080@30000/1001","name":null}"#);

    let over = Command::RecordStart {
        mode: RecordModeArg::OverSelection,
        countdown: None,
        wait: false,
    };
    insta::assert_snapshot!(json(&over), @r#"{"cmd":"recordStart","mode":"overSelection","countdown":null,"wait":false}"#);

    let scene = Command::SceneApply {
        from: SceneTarget::Project,
        to: SceneDest::Takes(vec![take(2)]),
    };
    insta::assert_snapshot!(json(&scene), @r#"{"cmd":"sceneApply","from":"project","to":{"takes":["take-0002"]}}"#);

    insta::assert_snapshot!(json(&Command::RecordStop), @r#"{"cmd":"recordStop"}"#);

    // The GUI's playback sync (GUI G§3.10 step 3).
    let synced = Command::Seek {
        to: SeekTarget::Player {
            frame: 2467,
            rev: 3,
        },
    };
    insta::assert_snapshot!(json(&synced), @r#"{"cmd":"seek","to":{"player":{"frame":2467,"rev":3}}}"#);
}

#[test]
fn command_parsing_rules() {
    // Absent / null / value for a Nullable field.
    let parse = |s: &str| serde_json::from_str::<Command>(s).unwrap();
    let Command::AutocutSettings { patch } = parse(r#"{"cmd":"autocutSettings","patch":{}}"#)
    else {
        panic!()
    };
    assert_eq!(patch.track, Nullable::Keep);
    let Command::AutocutSettings { patch } =
        parse(r#"{"cmd":"autocutSettings","patch":{"track":null}}"#)
    else {
        panic!()
    };
    assert_eq!(patch.track, Nullable::Clear);
    let Command::AutocutSettings { patch } =
        parse(r#"{"cmd":"autocutSettings","patch":{"track":"A2"}}"#)
    else {
        panic!()
    };
    assert_eq!(patch.track, Nullable::Set(TrackId::numbered(2)));

    // bool and Vec fields default; Options may be omitted.
    assert_eq!(
        parse(r#"{"cmd":"clipSplit","clip":"c7f3","at":"13f"}"#),
        Command::ClipSplit {
            clip: Some(clip("c7f3")),
            at: t("13f"),
            timeline: false
        }
    );
    assert_eq!(
        parse(r#"{"cmd":"clipRemove"}"#),
        Command::ClipRemove { clips: vec![] }
    );
    assert_eq!(
        parse(r#"{"cmd":"export"}"#),
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: None
        }
    );

    // Unknown fields, unknown commands and bad values are rejected.
    for bad in [
        r#"{"cmd":"clipSplit","clip":"c7f3","at":"13f","bogus":1}"#,
        r#"{"cmd":"clipSplat"}"#,
        r#"{"cmd":"clipSplit","clip":"c7f3","at":"13"}"#,
        r#"{"cmd":"layerOrder","target":"take1","layer":"L1","to":0}"#,
        r#"{"cmd":"setUi","patch":{"zom":1}}"#,
        r#"{"cmd":"layerAdd","target":"project","source":{"screen":{"output":"DP-1","extra":1}}}"#,
    ] {
        assert!(
            serde_json::from_str::<Command>(bad).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn queries_round_trip() {
    for q in [
        Query::Status,
        Query::State,
        Query::TakeList { unused_only: true },
        Query::TakeShow { take: take(3) },
        Query::ClipList,
        Query::History { limit: Some(20) },
        Query::History { limit: None },
        Query::Keys,
    ] {
        round_trip(&q);
    }
    assert_eq!(
        serde_json::from_str::<Query>(r#"{"query":"takeList"}"#).unwrap(),
        Query::TakeList { unused_only: false }
    );
}

fn record_mode_over() -> RecordMode {
    RecordMode::Over {
        clips: vec![ClipRef {
            id: clip("c7f3"),
            take: take(1),
            in_: 0,
            out: 410,
        }],
    }
}

#[test]
fn record_phases_round_trip() {
    let phases = [
        RecordPhase::Idle,
        RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: TimerId(7),
            start_req: Some(ReqId("r1".into())),
            wait: true,
        },
        RecordPhase::Starting {
            take: take(4),
            mode: record_mode_over(),
            start_req: None,
            wait: false,
        },
        RecordPhase::Recording {
            take: take(4),
            mode: RecordMode::Append,
            elapsed_ns: 1_500_000_000,
            waiters: vec![ReqId("r2".into())],
        },
        RecordPhase::Finalizing {
            take: take(4),
            mode: record_mode_over(),
            cancelled: false,
            stop_reqs: vec![ReqId("r3".into())],
            waiters: vec![],
            discard: Some(DiscardRequest {
                req: Some(ReqId("r4".into())),
            }),
        },
    ];
    for phase in &phases {
        round_trip(phase);
    }
    insta::assert_snapshot!(json(&phases[2]), @r#"{"phase":"starting","take":"take-0004","mode":{"over":{"clips":[{"id":"c7f3","take":"take-0001","in":0,"out":410}]}},"startReq":null,"wait":false}"#);
}

fn export_plan() -> ExportPlan {
    ExportPlan {
        out: "/home/me/exports/demo.mp4".into(),
        replace: Some(qrec_core::ctx::FileId {
            dev: 66_306,
            ino: 1_234_567,
        }),
        tmp_ext: "mp4".into(),
        segments: vec![SegmentPlan {
            clips: vec![clip("c7f3")],
            take: take(1),
            in_: 0,
            out: 410,
            frames: 410,
            samples: 656_000,
            key: "ab12".into(),
            path: ".qrec/cache/segments/ab12.mov".into(),
            argv: vec!["ffmpeg".into(), "file:x.mov".into()],
        }],
        stitch: StitchPlan {
            list_path: ".qrec/cache/segments/list.txt".into(),
            list_content: "file 'ab12.mov'\n".into(),
            argv: vec!["ffmpeg".into()],
        },
        total_frames: 410,
        files: BTreeSet::from(["takes/take-0001/L1-screen-DP-4.mkv".to_owned()]),
    }
}

fn capture_plan() -> CapturePlan {
    CapturePlan {
        take: take(4),
        dir: "takes/take-0004".into(),
        log: "takes/take-0004/recorder.log".into(),
        fps: Fps::new(30, 1),
        health_check_ms: 500,
        first_buffer_ms: 3000,
        eos_timeout_ms: 10_000,
        video: vec![VideoRecord {
            id: LayerId::numbered(2),
            source: VideoSource::Camera {
                device: "/dev/video0".into(),
                label: "C920".into(),
            },
            file: "takes/take-0004/L2-camera-0.mkv".into(),
            encoder: VideoEncoder::Va {
                factory: "vah264enc".into(),
                qp: 18,
            },
            key_int_max: 30,
            camera_mode: Some(qrec_core::ctx::CameraMode {
                width: 1280,
                height: 720,
                fps: Fps::new(30, 1),
                format: "image/jpeg".into(),
            }),
        }],
        audio: vec![AudioRecord {
            id: TrackId::numbered(1),
            device: "alsa_input.usb".into(),
            file: "takes/take-0004/A1-mic.mka".into(),
            sample_rate: 48_000,
        }],
    }
}

fn entry() -> HistoryEntry {
    HistoryEntry {
        seq: 3,
        ts: ts(),
        label: "Split clip c7f3 at 00:00:13:20".into(),
        cmd: json!({"cmd": "clipSplit", "clip": "c7f3", "at": "13f", "timeline": false}),
        forward: serde_json::from_value(
            json!([{"op": "replace", "path": "/timeline/clips/0/out", "value": 13}]),
        )
        .unwrap(),
        inverse: serde_json::from_value(json!([
            {"op": "test", "path": "/timeline/clips/0/out", "value": 13},
            {"op": "replace", "path": "/timeline/clips/0/out", "value": 410}
        ]))
        .unwrap(),
        after_hash: "00ff".into(),
    }
}

fn project() -> Project {
    serde_json::from_str(include_str!("fixtures/spec_example.json")).unwrap()
}

#[test]
fn effects_round_trip() {
    let effects = vec![
        Effect::WriteProject,
        Effect::WriteUi,
        Effect::History(HistoryOp::Append(entry())),
        Effect::History(HistoryOp::SetCursor(2)),
        Effect::History(HistoryOp::Rewrite {
            entries: vec![entry()],
            base: Box::new(project()),
            base_seq: 2,
            cursor: 3,
            next_seq: 4,
        }),
        Effect::CreateTakeDir {
            path: "takes/take-0004".into(),
        },
        Effect::DeleteTakeDirs {
            job: JobId(9),
            takes: vec![take(1)],
        },
        Effect::SetLive(None),
        Effect::SetLive(Some(LiveScene {
            canvas: project().canvas,
            scene: project().scene,
            preview: true,
        })),
        Effect::StartCapture {
            plan: capture_plan(),
        },
        Effect::StopCapture {
            take: take(4),
            cancel: true,
        },
        Effect::Probe {
            take: take(4),
            purpose: ProbePurpose::Finalize,
            jobs: vec![ProbeJob {
                id: "A1".into(),
                file: "takes/take-0004/A1-mic.mka".into(),
                kind: StreamKind::Audio,
                argv: qrec_core::media_argv::probe_argv(
                    "takes/take-0004/A1-mic.mka",
                    StreamKind::Audio,
                ),
            }],
        },
        Effect::DetectSilence {
            job: JobId(5),
            key: SilenceKey("k".into()),
            argv: vec!["ffmpeg".into()],
        },
        Effect::RenderExport {
            job: JobId(6),
            plan: export_plan(),
        },
        Effect::SaveFrame {
            job: JobId(7),
            plan: FramePlan {
                out: "/tmp/f.png".into(),
                replace: None,
                tmp_ext: "png".into(),
                argv: vec!["ffmpeg".into()],
                files: BTreeSet::new(),
            },
        },
        Effect::CancelJob { job: JobId(6) },
        Effect::StartTimer {
            timer: TimerId(8),
            after_ms: 1000,
        },
        Effect::CancelTimer { timer: TimerId(8) },
        Effect::Notify(
            Toast::new(
                ToastLevel::Success,
                "Layers! Like an onion. Your video is ready.",
            )
            .with_body("/home/me/exports/demo.mp4")
            .with_action(
                "Open file",
                UiAction::OpenPath("/home/me/exports/demo.mp4".into()),
            )
            .with_action(
                "Undo",
                UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
            ),
        ),
        Effect::Progress {
            req: ReqId("r1".into()),
            progress: Progress {
                phase: ProgressPhase::Render,
                done: 1234,
                total: 5678,
                eta_secs: Some(42),
                label: "Rendering clip 4/12".into(),
                take: None,
            },
        },
        Effect::Reply {
            req: ReqId("r1".into()),
            reply: Ok(CmdResult::new("Split clip c7f3")
                .with_seq(Some(12))
                .with_created(["c9b2"])),
        },
        Effect::Reply {
            req: ReqId("r2".into()),
            reply: Err(CoreError::not_found("no clip c000")),
        },
        Effect::Log {
            level: LogLevel::Debug,
            message: "ignored TimerFired".into(),
        },
    ];
    for effect in &effects {
        round_trip(effect);
    }
}

#[test]
fn events_round_trip() {
    let caps = Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8.1.2".into(),
            identity: "/usr/bin/ffmpeg".into(),
            encoders: BTreeSet::from(["libx264".to_owned()]),
        }),
        ffprobe: Some("/usr/bin/ffprobe".into()),
        vaapi: Some(VaapiCaps {
            device: "/dev/dri/renderD129".into(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: Some("varenderD129h264enc".into()),
        }),
        cores: 16,
    };
    let info = MediaInfo {
        kind: StreamKind::Video,
        frames: Some(912),
        fps: Some(Fps::new(30, 1)),
        width: Some(2560),
        height: Some(1440),
        sample_rate: None,
        channels: None,
        duration_ns: Some(30_400_000_000),
        start_time_ns: 0,
        color_range: Some("tv".into()),
        color_space: Some("bt709".into()),
    };
    let events = vec![
        Event::Capabilities(caps),
        Event::CaptureStarted {
            take: take(4),
            t0_ns: 123,
            files: vec!["takes/take-0004/A1-mic.mka".into()],
            start_offset_ns: BTreeMap::from([("A1".to_owned(), 3_906_126)]),
        },
        Event::CaptureFailed {
            take: Some(take(4)),
            source: Some("L2".into()),
            message: "device busy".into(),
            missing: None,
        },
        Event::CaptureFailed {
            take: None,
            source: None,
            message: "portal denied".into(),
            missing: None,
        },
        Event::CaptureFailed {
            take: Some(take(4)),
            source: Some("L1".into()),
            message: "the GStreamer element videotestsrc is missing".into(),
            missing: Some("videotestsrc".into()),
        },
        Event::StreamLost {
            take: take(4),
            stream: "L2".into(),
            message: "unplugged".into(),
        },
        Event::RecordClock {
            take: take(4),
            elapsed_ns: 250_000_000,
        },
        Event::CaptureStopped {
            take: take(4),
            streams: vec![RecordedStream {
                id: "L1".into(),
                file: "takes/take-0004/L1-screen-DP-4.mkv".into(),
                start_offset_ns: 41_000_000,
                eos: true,
            }],
            timed_out: false,
        },
        Event::EngineWedged {
            message: "teardown timed out".into(),
        },
        Event::TakeProbed {
            take: take(4),
            purpose: ProbePurpose::Recover,
            results: vec![
                ProbeResult {
                    id: "L1".into(),
                    file: "a.mkv".into(),
                    info: Ok(info),
                },
                ProbeResult {
                    id: "A1".into(),
                    file: "a.mka".into(),
                    info: Err("moov atom not found".into()),
                },
            ],
        },
        Event::TimerFired { timer: TimerId(8) },
        Event::SilenceDetected {
            job: JobId(5),
            key: SilenceKey("k".into()),
            silences: vec![Interval {
                start_ns: 0,
                end_ns: 500_000_000,
            }],
        },
        Event::SilenceFailed {
            job: JobId(5),
            key: SilenceKey("k".into()),
            message: "ffmpeg exited 1".into(),
        },
        Event::ExportProgress {
            job: JobId(6),
            segment: 2,
            frames_done: 100,
        },
        Event::ExportStitchProgress {
            job: JobId(6),
            out_time_ns: 5_000_000_000,
        },
        Event::ExportFinished {
            job: JobId(6),
            path: "/home/me/x.mp4".into(),
            bytes: 1 << 20,
        },
        Event::ExportFailed {
            job: JobId(6),
            message: "ffmpeg stalled".into(),
            stderr_tail: vec!["frame=10".into()],
            log: ".qrec/logs/x.log".into(),
        },
        Event::ExportCancelled { job: JobId(6) },
        Event::FrameSaved {
            job: JobId(7),
            path: "/tmp/f.png".into(),
        },
        Event::FrameFailed {
            job: JobId(7),
            message: "bad".into(),
            log: ".qrec/logs/y.log".into(),
        },
        Event::OutputAppeared {
            job: JobId(7),
            out: "/tmp/race.webm".into(),
            bytes: 20,
            kept: "/tmp/.race.qrec-part-1-7.webm".into(),
        },
        Event::TakeDirsDeleted {
            job: JobId(9),
            deleted: vec!["takes/take-0001".into()],
            errors: vec![],
        },
        Event::PurgeFailed {
            job: JobId(9),
            message: "write .qrec/qrec.json.tmp: No space left on device".into(),
        },
        Event::ProjectChangedOnDisk {
            project: Ok(Box::new(project())),
        },
        Event::ProjectChangedOnDisk {
            project: Err("expected `,` at line 3".into()),
        },
        Event::RequesterGone {
            req: ReqId("r1".into()),
        },
    ];
    for event in &events {
        round_trip(event);
    }
}

#[test]
fn state_and_outcome_round_trip() {
    let mut state = State::new(
        project(),
        History::new(),
        UiState::default(),
        HostKind::Headless,
    );
    state.history.append(entry());
    state.runtime.jobs.insert(
        JobId(6),
        Job {
            id: JobId(6),
            kind: JobKind::Export {
                out: "/home/me/x.mp4".into(),
                files: BTreeSet::from(["takes/take-0001/A1-mic.mka".to_owned()]),
                total_frames: 912,
                seg_frames: vec![410, 457],
                seg_done: vec![410, 0],
                seg_clips: Vec::new(),
                stitch_total_ns: 30_400_000_000,
                stitch_done_ns: 0,
                stitching: false,
            },
            req: Some(ReqId("r1".into())),
            started: ts(),
        },
    );
    state.runtime.jobs.insert(
        JobId(7),
        Job {
            id: JobId(7),
            kind: JobKind::Autocut {
                cmd: json!({"cmd": "autocut"}),
                targets: vec![AutocutTarget {
                    clip: ClipRef {
                        id: clip("c7f3"),
                        take: take(1),
                        in_: 0,
                        out: 410,
                    },
                    track: TrackId::numbered(1),
                    key: SilenceKey("k".into()),
                }],
                params: AutocutParams {
                    threshold_db: -35.0,
                    min_silence_ms: 400,
                    padding_ms: 120,
                    min_keep_ms: 200,
                },
                dry_run: false,
                pending: BTreeSet::from([SilenceKey("k".into())]),
                ready: false,
            },
            req: None,
            started: ts(),
        },
    );
    state.runtime.jobs.insert(
        JobId(8),
        Job {
            id: JobId(8),
            kind: JobKind::FrameSave {
                out: "/tmp/f.png".into(),
                files: BTreeSet::new(),
            },
            req: None,
            started: ts(),
        },
    );
    state.runtime.jobs.insert(
        JobId(9),
        Job {
            id: JobId(9),
            kind: JobKind::Purge {
                takes: vec![take(1)],
            },
            req: None,
            started: ts(),
        },
    );
    state.runtime.silence.insert(
        SilenceKey("k".into()),
        vec![Interval {
            start_ns: 1,
            end_ns: 2,
        }],
    );
    state.runtime.disk_invalid = Some("bad".into());
    state.ui.selection = vec![clip("c7f3")];
    state.ui.zoom = Some(1.5);
    state.ui.panels.insert("bin".into(), 300.0);
    round_trip(&state);

    let outcome = Outcome::new(
        state,
        vec![Effect::WriteProject],
        Some(Ok(CmdResult::new("ok"))),
    );
    round_trip(&outcome);

    let ctx = Ctx {
        now: ts(),
        utc_offset_secs: 7200,
        entropy: 42,
        confirmed: true,
        req: Some(ReqId("r1".into())),
        facts: Facts {
            folder_name: Some("demo".into()),
            min_take_number: 3,
            ..Default::default()
        },
    };
    round_trip(&ctx);
}

#[test]
fn views_and_plans_round_trip() {
    round_trip(&export_plan());
    round_trip(&capture_plan());
    round_trip(&GesTimeline {
        canvas: project().canvas,
        duration_ns: 1_000_000_000,
        video: vec![GesVideoClip {
            take: take(1),
            layer: LayerId::numbered(2),
            file: "a.mkv".into(),
            layer_priority: 1,
            start_ns: 0,
            inpoint_ns: 33_333_333,
            duration_ns: 1_000_000_000,
            placement: Placement {
                src: SourceRegion {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
                dst: Rect::new(0, 0, 1920, 1080),
            },
            alpha: 0.5,
        }],
        audio: vec![GesAudioClip {
            take: take(1),
            track: TrackId::numbered(1),
            file: "a.mka".into(),
            start_ns: 0,
            inpoint_ns: 0,
            duration_ns: 1_000_000_000,
            volume: 0.5,
            fade_in_samples: 240,
            fade_out_samples: 0,
        }],
    });
    round_trip(&VideoEncoder::X264 {
        pass: "qual".into(),
        quantizer: 18,
        speed_preset: "veryfast".into(),
        tune: "zerolatency".into(),
    });
    round_trip(&AutocutPlanView {
        clips: vec![AutocutClipView {
            clip: clip("c7f3"),
            kept: vec![[0, 120], [180, 410]],
            removed_frames: 60,
            silences: 1,
        }],
        skipped: vec![AutocutSkipView {
            clip: clip("c8a1"),
            reason: "changed while analyzing".into(),
        }],
    });
    round_trip(&TakeFinalizedView {
        take: take(4),
        status: TakeStatus::TooShort,
        duration_frames: 7,
        placed: false,
        clips: vec![],
    });
    round_trip(&RecordStartedView {
        take: take(4),
        mode: RecordMode::Append,
        files: vec!["a.mkv".into()],
    });
    round_trip(&StatusView {
        project: "demo".into(),
        path: None,
        initialized: true,
        host: HostKind::Gui,
        pid: Some(4242),
        record: RecordPhase::Idle,
        jobs: vec![],
        takes: 3,
        clips: 2,
        duration_frames: 867,
        cursor: 12,
    });
    round_trip(&ClipRow {
        id: clip("c7f3"),
        take: take(1),
        in_: 0,
        out: 410,
        start: 0,
        duration: 410,
    });
    round_trip(&HistoryRow {
        seq: 12,
        ts: ts(),
        label: "Split".into(),
        current: true,
    });
    round_trip(&TakeRow {
        id: take(1),
        status: TakeStatus::Ok,
        recorded_at: ts(),
        duration_frames: 912,
        clips: 2,
        unused: false,
        error: None,
        recovered: true,
    });
    round_trip(&ExportDone {
        path: "/x.mp4".into(),
        bytes: 10,
        frames: 912,
    });
    round_trip(&FrameSavedView {
        path: "/x.png".into(),
    });
}

#[test]
fn wire_examples_parse() {
    // The client lines of A§4.9.
    let lines = [
        r#"{"v":1,"id":"7","confirmed":false,"body":{"command":{"cmd":"clipSplit","clip":"c7f3","at":"13f"}}}"#,
        r#"{"v":1,"id":"8","confirmed":false,"body":{"query":{"query":"takeList","unusedOnly":true}}}"#,
        r#"{"v":1,"id":"9","body":"hello"}"#,
        r#"{"v":1,"id":"10","body":"shutdown"}"#,
    ];
    let requests: Vec<WireRequest> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        requests[0],
        WireRequest {
            v: WIRE_VERSION,
            id: "7".into(),
            confirmed: false,
            body: WireBody::Command(Command::ClipSplit {
                clip: Some(clip("c7f3")),
                at: t("13f"),
                timeline: false
            }),
        }
    );
    assert_eq!(
        requests[1].body,
        WireBody::Query(Query::TakeList { unused_only: true })
    );
    assert_eq!(requests[2].body, WireBody::Hello);
    assert!(!requests[2].confirmed);
    assert_eq!(requests[3].body, WireBody::Shutdown);
    for request in &requests {
        round_trip(request);
    }
    insta::assert_snapshot!(json(&requests[2]), @r#"{"v":1,"id":"9","confirmed":false,"body":"hello"}"#);

    // The host lines.
    let host = [
        r#"{"id":"7","accepted":true}"#,
        r#"{"id":"7","progress":{"phase":"render","done":1234,"total":5678,"etaSecs":42,"label":"Rendering clip 4/12"}}"#,
        r#"{"id":"7","reply":{"ok":true,"data":{"seq":12,"created":["c9b2"],"removed":[],"message":"…","result":null}}}"#,
        r#"{"id":"7","reply":{"ok":false,"error":{"code":"not_found","message":"…","hint":"…"}}}"#,
    ];
    let messages: Vec<WireMessage> = host
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(messages[0], WireMessage::accepted("7"));
    let WireMessage::Progress { progress, .. } = &messages[1] else {
        panic!("{:?}", messages[1])
    };
    assert_eq!(progress.eta_secs, Some(42));
    for (message, line) in messages.iter().zip(host) {
        assert_eq!(message.id(), "7");
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            serde_json::from_str::<serde_json::Value>(line).unwrap()
        );
    }
    let WireMessage::Reply { reply, .. } = &messages[2] else {
        panic!()
    };
    let data = from_wire(reply).unwrap().unwrap();
    let result: CmdResult = serde_json::from_value(data).unwrap();
    assert_eq!(result.seq, Some(12));
    let WireMessage::Reply { reply, .. } = &messages[3] else {
        panic!()
    };
    let err = from_wire(reply).unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // to_wire is the --json object.
    let ok = to_wire(&Ok(json!({"seq": 1})));
    insta::assert_snapshot!(ok.to_string(), @r#"{"data":{"seq":1},"ok":true}"#);
    let err = to_wire(&Err(CoreError::busy("recording take 3")));
    insta::assert_snapshot!(err.to_string(), @r#"{"error":{"code":"busy","message":"recording take 3"},"ok":false}"#);

    let hello = HelloInfo {
        pid: 4242,
        project_dir: "/home/me/demo".into(),
        project_dir_hex: None,
        host: HostKind::Headless,
        record: RecordPhase::Idle,
        jobs: vec![],
        version: "0.1.0".into(),
    };
    round_trip(&hello);
    insta::assert_snapshot!(json(&hello), @r#"{"pid":4242,"projectDir":"/home/me/demo","host":"headless","record":{"phase":"idle"},"jobs":[],"version":"0.1.0"}"#);
}

#[test]
fn schemas_generate() {
    let command = serde_json::to_string(&schemars::schema_for!(Command)).unwrap();
    assert!(
        command.contains("vaapiDevice") && command.contains("take-0001"),
        "{command}"
    );
    for schema in [
        serde_json::to_value(schemars::schema_for!(Query)).unwrap(),
        serde_json::to_value(schemars::schema_for!(WireRequest)).unwrap(),
        serde_json::to_value(schemars::schema_for!(WireMessage)).unwrap(),
        serde_json::to_value(schemars::schema_for!(HelloInfo)).unwrap(),
        serde_json::to_value(schemars::schema_for!(CmdResult)).unwrap(),
        serde_json::to_value(schemars::schema_for!(StatusView)).unwrap(),
        serde_json::to_value(schemars::schema_for!(Toast)).unwrap(),
        serde_json::to_value(schemars::schema_for!(Runtime)).unwrap(),
        serde_json::to_value(schemars::schema_for!(UiState)).unwrap(),
        serde_json::to_value(schemars::schema_for!(ExportPlan)).unwrap(),
        serde_json::to_value(schemars::schema_for!(Project)).unwrap(),
    ] {
        assert!(schema.is_object());
    }
}
