//! UI-state commands: selection, playhead, playback and view. Owner: timeline.
//!
//! These change `State::ui` / `runtime.playing` and finish with
//! `history::persist` (never undoable). `apply` already rejects the G
//! commands (`Seek`, `Play`, `Pause`, `Shuttle`) outside a GUI host.
//!
//! Whenever the live pipeline a state wants changes ([`live_scene`], A§8.11:
//! the GUI's Live canvas, or a take being counted down, recorded or
//! finalized), the handler also emits `SetLive` with the new wish.
//!
//! Playback renders on the Program canvas: `Play` and `Shuttle` switch the
//! canvas to Program, and switching to Live stops playback.

use serde_json::json;

use crate::apply::Outcome;
use crate::apply::clip::describe;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::history::persist;
use crate::ids::{ClipId, Frame};
use crate::plans::LiveScene;
use crate::protocol::command::{Nullable, SceneTarget, SeekTarget, SelectOp, ShuttleDir, UiPatch};
use crate::protocol::effect::Effect;
use crate::protocol::reply::CmdResult;
use crate::state::{CanvasMode, HostKind, State};
use crate::time::timecode;
use crate::timeline;
use crate::ui_normalize::{canvas_scene, canvas_target};

/// `Select` (R): click, Shift+click (`RangeTo`), Ctrl+click (`Toggle`),
/// `,`/`.` (`Prev`/`Next`), Ctrl+A (`All`), Esc (`Clear`).
///
/// `RangeTo(c)` selects every clip from the current selection through `c`
/// (the smallest run containing both). `Prev`/`Next` select the clip before
/// the first / after the last selected clip, clamped to the timeline; with
/// nothing selected they select the clip under the playhead (the last clip
/// when the playhead is at the end).
pub fn select(state: &State, _ctx: &Ctx, op: SelectOp) -> Result<Outcome, CoreError> {
    let clips = &state.project.timeline.clips;
    let find =
        |id: &ClipId| timeline::index_of(clips, id).ok_or_else(|| timeline::unknown_clip(id));
    let mut current: Vec<usize> = state
        .ui
        .selection
        .iter()
        .filter_map(|id| timeline::index_of(clips, id))
        .collect();
    current.sort_unstable();
    let ids = |range: std::ops::RangeInclusive<usize>| -> Vec<ClipId> {
        clips[range].iter().map(|c| c.id.clone()).collect()
    };
    let selection = match op {
        SelectOp::Set(wanted) => {
            for id in &wanted {
                find(id)?;
            }
            wanted
        }
        SelectOp::Toggle(id) => {
            find(&id)?;
            let mut selection = state.ui.selection.clone();
            if selection.contains(&id) {
                selection.retain(|s| s != &id);
            } else {
                selection.push(id);
            }
            selection
        }
        SelectOp::RangeTo(id) => {
            let i = find(&id)?;
            match (current.first(), current.last()) {
                (Some(&first), Some(&last)) => ids(first.min(i)..=last.max(i)),
                _ => vec![id],
            }
        }
        SelectOp::Prev | SelectOp::Next if clips.is_empty() => Vec::new(),
        SelectOp::Prev => {
            let i = current
                .first()
                .map_or_else(|| clip_under_playhead(state), |&f| f.saturating_sub(1));
            ids(i..=i)
        }
        SelectOp::Next => {
            let i = current.last().map_or_else(
                || clip_under_playhead(state),
                |&l| (l + 1).min(clips.len() - 1),
            );
            ids(i..=i)
        }
        SelectOp::All => clips.iter().map(|c| c.id.clone()).collect(),
        SelectOp::Clear => Vec::new(),
    };
    let mut next = state.clone();
    next.ui.selection = selection;
    Ok(finish(state, next, |s| {
        let selection = &s.ui.selection;
        let message = if selection.is_empty() {
            if s.project.timeline.clips.is_empty() {
                "Nothing selected: the timeline is empty".to_owned()
            } else {
                "Nothing selected".to_owned()
            }
        } else {
            format!("Selected {}", describe(selection))
        };
        CmdResult::new(message).with_result(&json!({ "selection": selection }))
    }))
}

/// `qrec seek` (G, R): `To` is a timeline frame, `By` a relative move
/// (an unsigned value counts as `+`); `EditPrev`/`EditNext` jump to the
/// previous/next clip boundary. The playhead stays within `[0, total]`.
pub fn seek(state: &State, _ctx: &Ctx, to: SeekTarget) -> Result<Outcome, CoreError> {
    let fps = state.project.canvas.fps;
    let clips = &state.project.timeline.clips;
    let total = timeline::total_frames(clips);
    let here = state.ui.playhead.min(total);
    let target: Frame = match &to {
        SeekTarget::To(arg) => timeline::absolute_frame(arg, fps, "seek --to")?.min(total),
        SeekTarget::By(arg) => {
            let delta = timeline::delta_frames(arg, fps)?;
            (i128::from(here) + i128::from(delta)).clamp(0, i128::from(total)) as Frame
        }
        SeekTarget::EditPrev => timeline::edit_points(clips)
            .into_iter()
            .rev()
            .find(|&p| p < here)
            .unwrap_or(0),
        SeekTarget::EditNext => timeline::edit_points(clips)
            .into_iter()
            .find(|&p| p > here)
            .unwrap_or(total),
        SeekTarget::Start => 0,
        SeekTarget::End => total,
    };
    let mut next = state.clone();
    next.ui.playhead = target;
    Ok(finish(state, next, |s| {
        let at = timecode(s.ui.playhead, fps);
        CmdResult::new(format!("Playhead at {at}"))
            .with_result(&json!({ "playhead": s.ui.playhead, "timecode": at }))
    }))
}

/// `qrec play` (G): plays the timeline on the Program canvas (switching to
/// it), from the start when the playhead is at the end. `Invalid` on an
/// empty timeline.
pub fn play(state: &State, _ctx: &Ctx) -> Result<Outcome, CoreError> {
    start_playing(state, false)
}

/// `qrec pause` (G, R).
pub fn pause(state: &State, _ctx: &Ctx) -> Result<Outcome, CoreError> {
    let mut next = state.clone();
    next.runtime.playing = false;
    let was_playing = state.runtime.playing;
    Ok(finish(state, next, |s| {
        let at = timecode(s.ui.playhead, s.project.canvas.fps);
        let message = if was_playing {
            format!("Paused at {at}")
        } else {
            format!("Already paused at {at}")
        };
        CmdResult::new(message).with_result(&playback_view(s, false))
    }))
}

/// `j` / `k` / `l` (G): play backwards / stop / play forwards. The core
/// tracks only whether playback runs; the GUI's player owns the speed
/// (press again for 2×, 4×). Backwards from the very start just stops.
pub fn shuttle(state: &State, ctx: &Ctx, dir: ShuttleDir) -> Result<Outcome, CoreError> {
    match dir {
        ShuttleDir::Reverse => start_playing(state, true),
        ShuttleDir::Stop => pause(state, ctx),
        ShuttleDir::Forward => start_playing(state, false),
    }
}

/// `Tab` (R): Live shows the live sources with the project scene, Program
/// the timeline at the playhead. Leaving Program stops playback; the live
/// pipeline follows (`SetLive`).
pub fn set_canvas_mode(state: &State, _ctx: &Ctx, mode: CanvasMode) -> Result<Outcome, CoreError> {
    let mut next = state.clone();
    next.ui.canvas_mode = mode;
    if mode == CanvasMode::Live {
        next.runtime.playing = false;
    }
    let name = match mode {
        CanvasMode::Live => "Live",
        CanvasMode::Program => "Program",
    };
    let message = if state.ui.canvas_mode == mode {
        format!("The canvas is already in {name} mode")
    } else {
        format!("Switched the canvas to {name}")
    };
    Ok(finish(state, next, |_| {
        CmdResult::new(message).with_result(&json!({ "canvasMode": mode }))
    }))
}

/// Zoom, scroll, panels, bin, snapping, solo, selected layer (R). Absent
/// fields are kept; `panels` entries are merged into the stored ones.
/// `solo` and `selected_layer` must exist in the scene the canvas targets.
pub fn set_ui(state: &State, _ctx: &Ctx, patch: UiPatch) -> Result<Outcome, CoreError> {
    let mut ui = state.ui.clone();
    let mut changed: Vec<&str> = Vec::new();
    match patch.zoom {
        Nullable::Keep => {}
        Nullable::Clear => {
            ui.zoom = None;
            changed.push("zoom");
        }
        Nullable::Set(zoom) => {
            if !(zoom.is_finite() && zoom > 0.0) {
                return Err(CoreError::usage(format!(
                    "zoom must be a positive number of pixels per frame, not {zoom}"
                ))
                .with_hint("send null to fit the whole timeline"));
            }
            ui.zoom = Some(zoom);
            changed.push("zoom");
        }
    }
    if let Some(px) = patch.scroll_px {
        if !px.is_finite() {
            return Err(CoreError::usage(format!(
                "scroll must be a number of pixels, not {px}"
            )));
        }
        ui.scroll_px = px.max(0.0);
        changed.push("scroll");
    }
    if let Some(open) = patch.bin_open {
        ui.bin_open = open;
        changed.push("bin");
    }
    if let Some(on) = patch.snapping {
        ui.snapping = on;
        changed.push("snapping");
    }
    let scene = canvas_scene(&state.project, &state.ui);
    let scene_name = || match canvas_target(&state.project, &state.ui) {
        SceneTarget::Project => "the project scene".to_owned(),
        SceneTarget::Take(take) => format!("take {}'s scene", take.number()),
    };
    match patch.solo {
        Nullable::Keep => {}
        Nullable::Clear => {
            ui.solo = None;
            changed.push("solo");
        }
        Nullable::Set(track) => {
            if scene.track(&track).is_none() {
                return Err(CoreError::not_found(format!(
                    "no audio track {track} in {}",
                    scene_name()
                )));
            }
            ui.solo = Some(track);
            changed.push("solo");
        }
    }
    match patch.selected_layer {
        Nullable::Keep => {}
        Nullable::Clear => {
            ui.selected_layer = None;
            changed.push("selected layer");
        }
        Nullable::Set(layer) => {
            if scene.layer(&layer).is_none() {
                return Err(CoreError::not_found(format!(
                    "no layer {layer} in {}",
                    scene_name()
                )));
            }
            ui.selected_layer = Some(layer);
            changed.push("selected layer");
        }
    }
    if let Some(panels) = patch.panels {
        if let Some((name, size)) = panels.iter().find(|(_, size)| !size.is_finite()) {
            return Err(CoreError::usage(format!(
                "panel {name:?} must have a finite size, not {size}"
            )));
        }
        ui.panels.extend(panels);
        changed.push("panels");
    }
    let message = if changed.is_empty() {
        "Nothing to change".to_owned()
    } else {
        format!("Updated {}", changed.join(", "))
    };
    let mut next = state.clone();
    next.ui = ui;
    Ok(finish(state, next, |s| {
        CmdResult::new(message).with_result(&json!({ "ui": s.ui }))
    }))
}

/// The live pipeline `state` wants (A§8.11): the project scene while the
/// GUI shows the Live canvas (with the preview branch), or while a take is
/// counted down, recorded or finalized in any host (record bins attach to
/// it; no preview unless the GUI is in Live mode). `None` otherwise.
pub fn live_scene(state: &State) -> Option<LiveScene> {
    let preview = state.runtime.host == HostKind::Gui && state.ui.canvas_mode == CanvasMode::Live;
    let recording = !state.runtime.record.is_idle();
    (preview || recording).then(|| LiveScene {
        canvas: state.project.canvas.clone(),
        scene: state.project.scene.clone(),
        preview,
    })
}

/// `SetLive` with what `next` wants, when that differs from what `prev`
/// wanted. Every step that changes the canvas mode, the record phase or the
/// project scene can use it.
pub fn live_effect(prev: &State, next: &State) -> Option<Effect> {
    let wanted = live_scene(next);
    (live_scene(prev) != wanted).then_some(Effect::SetLive(wanted))
}

/// Persists a UI change (which also emits the `SetLive` it implies) and
/// replies with what `reply` says about the persisted state.
fn finish(prev: &State, next: State, reply: impl FnOnce(&State) -> CmdResult) -> Outcome {
    let committed = persist(prev, next);
    let result = reply(&committed.state);
    Outcome::done(committed, result)
}

/// `Play`, `Shuttle` forward or backwards.
fn start_playing(state: &State, reverse: bool) -> Result<Outcome, CoreError> {
    let total = timeline::total_frames(&state.project.timeline.clips);
    if total == 0 {
        return Err(CoreError::invalid("nothing to play: the timeline is empty")
            .with_hint("record a take first (r, or qrec record start)"));
    }
    let mut next = state.clone();
    let here = next.ui.playhead.min(total);
    if reverse && here == 0 {
        next.runtime.playing = false;
        return Ok(finish(state, next, |s| {
            CmdResult::new("At the start of the timeline; nothing to play backwards")
                .with_result(&playback_view(s, true))
        }));
    }
    if !reverse && here == total {
        next.ui.playhead = 0;
    }
    next.ui.canvas_mode = CanvasMode::Program;
    next.runtime.playing = true;
    Ok(finish(state, next, |s| {
        let at = timecode(s.ui.playhead, s.project.canvas.fps);
        let message = if reverse {
            format!("Playing backwards from {at}")
        } else {
            format!("Playing from {at}")
        };
        CmdResult::new(message).with_result(&playback_view(s, reverse))
    }))
}

fn playback_view(state: &State, reverse: bool) -> serde_json::Value {
    json!({
        "playing": state.runtime.playing,
        "reverse": reverse && state.runtime.playing,
        "playhead": state.ui.playhead,
        "canvasMode": state.ui.canvas_mode,
    })
}

/// The clip under the playhead, or the last clip at the end (non-empty timeline).
fn clip_under_playhead(state: &State) -> usize {
    let clips = &state.project.timeline.clips;
    timeline::clip_at(clips, state.ui.playhead).map_or(clips.len().saturating_sub(1), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::*;
    use crate::ErrorCode;
    use crate::apply::apply;
    use crate::ids::{LayerId, TakeId, TrackId};
    use crate::protocol::command::Command;
    use crate::state::{RecordMode, RecordPhase};
    use crate::testkit::{self, ProjectBuilder};
    use crate::time::TimeArg;

    const NOW: &str = "2026-10-14T12:30:00Z";

    /// Clips c001 [0, 100), c002 [100, 250), c003 [250, 450) on the
    /// timeline; GUI host, Live canvas, playhead at 0.
    fn state() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .clip("c001", "take-0001", 0, 100)
                .clip("c002", "take-0001", 150, 300)
                .clip("c003", "take-0002", 0, 200)
                .build(),
        )
    }

    fn run(state: &State, cmd: Command) -> Result<Outcome, CoreError> {
        apply(state, &testkit::ctx(NOW, 1), cmd)
    }

    fn cid(s: &str) -> ClipId {
        s.parse().unwrap()
    }

    fn t(s: &str) -> TimeArg {
        s.parse().unwrap()
    }

    fn summary(out: &Outcome) -> Value {
        json!({
            "reply": out.reply,
            "effects": out.effects,
            "ui": out.state.ui,
            "playing": out.state.runtime.playing,
        })
    }

    fn selected(state: &State, op: SelectOp) -> Vec<String> {
        let out = run(state, Command::Select { op }).unwrap();
        out.state
            .ui
            .selection
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn with_selection(ids: &[&str]) -> State {
        let mut s = state();
        s.ui.selection = ids.iter().map(|i| cid(i)).collect();
        s
    }

    #[test]
    fn select_ops() {
        let none = state();
        let out = run(
            &none,
            Command::Select {
                op: SelectOp::Set(vec![cid("c003"), cid("c001")]),
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("select_set", summary(&out));

        assert_eq!(selected(&none, SelectOp::All), ["c001", "c002", "c003"]);
        assert!(selected(&with_selection(&["c002"]), SelectOp::Clear).is_empty());
        assert_eq!(
            selected(&with_selection(&["c002"]), SelectOp::Toggle(cid("c001"))),
            ["c001", "c002"]
        );
        assert_eq!(
            selected(
                &with_selection(&["c001", "c002"]),
                SelectOp::Toggle(cid("c001"))
            ),
            ["c002"]
        );
        assert_eq!(selected(&none, SelectOp::RangeTo(cid("c002"))), ["c002"]);
        assert_eq!(
            selected(&with_selection(&["c003"]), SelectOp::RangeTo(cid("c001"))),
            ["c001", "c002", "c003"]
        );
        assert_eq!(
            selected(&with_selection(&["c002"]), SelectOp::RangeTo(cid("c003"))),
            ["c002", "c003"]
        );
        // Prev / Next move a single selection and clamp at the ends.
        assert_eq!(
            selected(&with_selection(&["c002"]), SelectOp::Prev),
            ["c001"]
        );
        assert_eq!(
            selected(&with_selection(&["c001"]), SelectOp::Prev),
            ["c001"]
        );
        assert_eq!(
            selected(&with_selection(&["c002", "c003"]), SelectOp::Next),
            ["c003"]
        );
        assert_eq!(
            selected(&with_selection(&["c002", "c003"]), SelectOp::Prev),
            ["c001"]
        );
        // With nothing selected: the clip under the playhead, the last one at the end.
        let mut s = state();
        s.ui.playhead = 120;
        assert_eq!(selected(&s, SelectOp::Next), ["c002"]);
        assert_eq!(selected(&s, SelectOp::Prev), ["c002"]);
        s.ui.playhead = 450;
        assert_eq!(selected(&s, SelectOp::Prev), ["c003"]);

        let empty = testkit::state(ProjectBuilder::new().build());
        let out = run(&empty, Command::Select { op: SelectOp::Next }).unwrap();
        assert_eq!(
            out.reply.unwrap().unwrap().message,
            "Nothing selected: the timeline is empty"
        );
        assert!(out.effects.is_empty());

        let err = run(
            &none,
            Command::Select {
                op: SelectOp::Set(vec![cid("c001"), cid("c404")]),
            },
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotFound, "no clip c404 on the timeline")
        );
        assert_eq!(
            run(
                &none,
                Command::Select {
                    op: SelectOp::Toggle(cid("c404"))
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            run(
                &none,
                Command::Select {
                    op: SelectOp::RangeTo(cid("c404"))
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn select_works_while_recording() {
        let mut s = state();
        s.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(selected(&s, SelectOp::All).len(), 3);
    }

    #[test]
    fn seek_targets() {
        let s = state();
        let seek =
            |s: &State, to: SeekTarget| run(s, Command::Seek { to }).map(|o| o.state.ui.playhead);
        assert_eq!(seek(&s, SeekTarget::To(t("00:00:04:00"))).unwrap(), 120);
        assert_eq!(
            seek(&s, SeekTarget::To(t("1000f"))).unwrap(),
            450,
            "clamped to the end"
        );
        assert_eq!(
            seek(&s, SeekTarget::To(t("-1f"))).unwrap_err().code,
            ErrorCode::Usage
        );
        assert_eq!(seek(&s, SeekTarget::End).unwrap(), 450);

        let mut at = s.clone();
        at.ui.playhead = 120;
        assert_eq!(seek(&at, SeekTarget::By(t("-1s"))).unwrap(), 90);
        assert_eq!(seek(&at, SeekTarget::By(t("+1f"))).unwrap(), 121);
        assert_eq!(seek(&at, SeekTarget::By(t("2f"))).unwrap(), 122);
        assert_eq!(seek(&at, SeekTarget::By(t("-1000f"))).unwrap(), 0);
        assert_eq!(seek(&at, SeekTarget::By(t("+1000f"))).unwrap(), 450);
        assert_eq!(seek(&at, SeekTarget::EditPrev).unwrap(), 100);
        assert_eq!(seek(&at, SeekTarget::EditNext).unwrap(), 250);
        assert_eq!(seek(&at, SeekTarget::Start).unwrap(), 0);
        at.ui.playhead = 100;
        assert_eq!(
            seek(&at, SeekTarget::EditPrev).unwrap(),
            0,
            "strictly before"
        );
        assert_eq!(
            seek(&at, SeekTarget::EditNext).unwrap(),
            250,
            "strictly after"
        );
        at.ui.playhead = 450;
        assert_eq!(seek(&at, SeekTarget::EditNext).unwrap(), 450);
        at.ui.playhead = 0;
        assert_eq!(seek(&at, SeekTarget::EditPrev).unwrap(), 0);

        let out = run(
            &s,
            Command::Seek {
                to: SeekTarget::To(t("13.7s")),
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("seek_to", summary(&out));
        let empty = testkit::state(ProjectBuilder::new().build());
        assert_eq!(seek(&empty, SeekTarget::End).unwrap(), 0);
        assert_eq!(seek(&empty, SeekTarget::EditNext).unwrap(), 0);
    }

    #[test]
    fn transport_needs_the_gui_host() {
        for host in [HostKind::Cli, HostKind::Headless] {
            let mut s = state();
            s.runtime.host = host;
            for cmd in [
                Command::Seek {
                    to: SeekTarget::Start,
                },
                Command::Play,
                Command::Pause,
                Command::Shuttle {
                    dir: ShuttleDir::Forward,
                },
            ] {
                let err = run(&s, cmd.clone()).unwrap_err();
                assert_eq!(err.code, ErrorCode::Unavailable, "{cmd:?} in {host:?}");
            }
            // The rest of the UI commands work in every host.
            assert!(run(&s, Command::Select { op: SelectOp::All }).is_ok());
            assert!(
                run(
                    &s,
                    Command::SetCanvasMode {
                        mode: CanvasMode::Program
                    }
                )
                .is_ok()
            );
            assert!(
                run(
                    &s,
                    Command::SetUi {
                        patch: UiPatch {
                            snapping: Some(false),
                            ..UiPatch::default()
                        }
                    }
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn play_switches_to_program() {
        let mut s = state();
        s.ui.playhead = 450;
        let out = run(&s, Command::Play).unwrap();
        // From the end it starts over; the Live pipeline isn't wanted anymore.
        insta::assert_json_snapshot!("play_from_live", summary(&out));

        let playing = out.state;
        let out = run(&playing, Command::Pause).unwrap();
        assert!(!out.state.runtime.playing);
        assert_eq!(
            out.effects,
            [],
            "playing isn't persisted; Program keeps the pipeline off"
        );
        assert_eq!(out.reply.unwrap().unwrap().message, "Paused at 00:00:00:00");
        let out = run(&s, Command::Pause).unwrap();
        assert_eq!(
            out.reply.unwrap().unwrap().message,
            "Already paused at 00:00:15:00"
        );

        let empty = testkit::state(ProjectBuilder::new().build());
        let err = run(&empty, Command::Play).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::Invalid, "nothing to play: the timeline is empty")
        );
    }

    #[test]
    fn shuttle_directions() {
        let mut s = state();
        s.ui.canvas_mode = CanvasMode::Program;
        s.ui.playhead = 200;
        let out = run(
            &s,
            Command::Shuttle {
                dir: ShuttleDir::Reverse,
            },
        )
        .unwrap();
        assert!(out.state.runtime.playing);
        assert_eq!(
            out.reply.unwrap().unwrap().message,
            "Playing backwards from 00:00:06:20"
        );
        let out = run(
            &s,
            Command::Shuttle {
                dir: ShuttleDir::Forward,
            },
        )
        .unwrap();
        assert!(out.state.runtime.playing);
        assert_eq!(out.state.ui.playhead, 200);
        let stopped = run(
            &out.state,
            Command::Shuttle {
                dir: ShuttleDir::Stop,
            },
        )
        .unwrap();
        assert!(!stopped.state.runtime.playing);
        // Backwards from the very start just stops.
        let mut start = out.state.clone();
        start.ui.playhead = 0;
        let out = run(
            &start,
            Command::Shuttle {
                dir: ShuttleDir::Reverse,
            },
        )
        .unwrap();
        assert!(!out.state.runtime.playing);
        assert_eq!(
            out.reply.unwrap().unwrap().message,
            "At the start of the timeline; nothing to play backwards"
        );
    }

    #[test]
    fn canvas_mode_drives_the_live_pipeline() {
        let s = state();
        let program = run(
            &s,
            Command::SetCanvasMode {
                mode: CanvasMode::Program,
            },
        )
        .unwrap();
        assert_eq!(program.effects, [Effect::WriteUi, Effect::SetLive(None)]);
        let live = run(
            &program.state,
            Command::SetCanvasMode {
                mode: CanvasMode::Live,
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("canvas_to_live", summary(&live));
        let again = run(
            &live.state,
            Command::SetCanvasMode {
                mode: CanvasMode::Live,
            },
        )
        .unwrap();
        assert!(again.effects.is_empty());
        assert_eq!(
            again.reply.unwrap().unwrap().message,
            "The canvas is already in Live mode"
        );

        // Switching to Live stops playback.
        let mut playing = program.state.clone();
        playing.runtime.playing = true;
        let out = run(
            &playing,
            Command::SetCanvasMode {
                mode: CanvasMode::Live,
            },
        )
        .unwrap();
        assert!(!out.state.runtime.playing);

        // While recording the pipeline stays up; only the preview branch follows the mode.
        let mut rec = s.clone();
        rec.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        let out = run(
            &rec,
            Command::SetCanvasMode {
                mode: CanvasMode::Program,
            },
        )
        .unwrap();
        let Some(Effect::SetLive(Some(scene))) = out.effects.last() else {
            panic!("expected SetLive(Some): {:?}", out.effects);
        };
        assert!(!scene.preview);
        assert_eq!(scene.scene, rec.project.scene);
        // A headless host has no preview: its wish doesn't change with the mode.
        rec.runtime.host = HostKind::Headless;
        let out = run(
            &rec,
            Command::SetCanvasMode {
                mode: CanvasMode::Program,
            },
        )
        .unwrap();
        assert_eq!(out.effects, [Effect::WriteUi]);
        assert_eq!(live_scene(&rec).map(|l| l.preview), Some(false));
        rec.runtime.record = RecordPhase::Idle;
        assert_eq!(live_scene(&rec), None);
    }

    #[test]
    fn set_ui_patches_and_validates() {
        let s = state();
        let mut panels = BTreeMap::new();
        panels.insert("layers".to_owned(), 352.0);
        let patch = UiPatch {
            zoom: Nullable::Set(2.5),
            scroll_px: Some(-3.0),
            bin_open: Some(true),
            snapping: Some(false),
            solo: Nullable::Set(TrackId::numbered(1)),
            selected_layer: Nullable::Set(LayerId::numbered(1)),
            panels: Some(panels),
        };
        let out = run(&s, Command::SetUi { patch }).unwrap();
        insta::assert_json_snapshot!("set_ui_all_fields", summary(&out));

        // Panels merge; null clears.
        let mut more = BTreeMap::new();
        more.insert("bin".to_owned(), 300.0);
        let patch = UiPatch {
            zoom: Nullable::Clear,
            solo: Nullable::Clear,
            panels: Some(more),
            ..UiPatch::default()
        };
        let out = run(&out.state, Command::SetUi { patch }).unwrap();
        assert_eq!(out.state.ui.zoom, None);
        assert_eq!(out.state.ui.solo, None);
        assert_eq!(
            out.state.ui.panels.keys().collect::<Vec<_>>(),
            ["bin", "layers"]
        );
        let out = run(
            &out.state,
            Command::SetUi {
                patch: UiPatch::default(),
            },
        )
        .unwrap();
        assert!(out.effects.is_empty());
        assert_eq!(out.reply.unwrap().unwrap().message, "Nothing to change");

        let bad = |patch: UiPatch| run(&s, Command::SetUi { patch }).unwrap_err();
        assert_eq!(
            bad(UiPatch {
                zoom: Nullable::Set(0.0),
                ..UiPatch::default()
            })
            .code,
            ErrorCode::Usage
        );
        assert_eq!(
            bad(UiPatch {
                zoom: Nullable::Set(f64::NAN),
                ..UiPatch::default()
            })
            .code,
            ErrorCode::Usage
        );
        assert_eq!(
            bad(UiPatch {
                scroll_px: Some(f64::INFINITY),
                ..UiPatch::default()
            })
            .code,
            ErrorCode::Usage
        );
        let mut panels = BTreeMap::new();
        panels.insert("x".to_owned(), f64::NAN);
        assert_eq!(
            bad(UiPatch {
                panels: Some(panels),
                ..UiPatch::default()
            })
            .code,
            ErrorCode::Usage
        );
        let err = bad(UiPatch {
            solo: Nullable::Set(TrackId::numbered(7)),
            ..UiPatch::default()
        });
        assert_eq!(
            (err.code, err.message.as_str()),
            (
                ErrorCode::NotFound,
                "no audio track A7 in the project scene"
            )
        );
        let mut program = s.clone();
        program.ui.canvas_mode = CanvasMode::Program;
        program.ui.selection = vec![cid("c003")];
        let err = run(
            &program,
            Command::SetUi {
                patch: UiPatch {
                    selected_layer: Nullable::Set(LayerId::numbered(9)),
                    ..UiPatch::default()
                },
            },
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotFound, "no layer L9 in take 2's scene")
        );
    }
}
