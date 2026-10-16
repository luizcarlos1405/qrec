//! The header bar (G§4.4, C04, C01 Record, C05): title and subtitle, the
//! record button, the export slot, the mode switch. Owner: WP6.
//!
//! The record button never moves or resizes (DESIGN: 120×32 in every
//! state); what it shows follows the record phase (D-04: `■ Stop [R]` from
//! Starting on). The export slot shows `Export…`, or a running export's
//! percent with a mini mud bar (whoever started it). The mode switch is
//! always enabled (R-allowed, A§5.3), so it has no view here.

use serde::{Deserialize, Serialize};

use crate::keymap::{KeyChord, Keymap, usable_keys};
use crate::on_event::output::export_readout;
use crate::state::{JobKind, RecordPhase, State};
use crate::view::{GuiFacts, format};

/// Everything the header shows.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderView {
    /// `project.name`.
    pub title: String,
    pub subtitle: Subtitle,
    pub record: RecordButton,
    pub export: ExportSlot,
    /// The 2 px `record` line under the header (recording, finalizing).
    pub record_line: bool,
    /// The narrow layout's layers button (G§3.15): shown exactly when
    /// [`header`] is told the layout is narrow.
    pub layers_button: bool,
}

/// The subtitle under the title.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Subtitle {
    /// "No clips yet".
    #[default]
    NoClips,
    /// `format::clips_summary`: "10 clips · " and "00:04:06:11".
    Clips { count: String, duration: String },
    /// "Recovering take 7…" with a 12 px spinner (D-47).
    Recovering { text: String },
}

/// The record button's look (C01 Record).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordButtonKind {
    /// `● Record [R]`.
    #[default]
    Record,
    /// Outlined `Cancel [R]` during the countdown.
    Cancel,
    /// `■ Stop [R]`, starting or recording.
    Stop,
    /// `Saving…` with a spinner, finalizing.
    Saving,
}

/// The record button.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordButton {
    pub kind: RecordButtonKind,
    pub enabled: bool,
    /// The key caps of `record.toggle` (`[["R"]]`); empty when unbound.
    /// The button shows them in every state; the tooltip names them only
    /// while the button is enabled.
    pub keys: Vec<Vec<String>>,
    /// "Record", "Add a layer or audio track first", …
    pub tooltip: String,
}

/// The export slot: the `Export…` button, or the running export's
/// progress (`export_readout`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSlot {
    /// `Some(42)` while an export runs: "42%" and the mini mud bar.
    pub percent: Option<u8>,
    pub enabled: bool,
    /// Pressed while the export panel is open.
    pub pressed: bool,
    /// "Exporting 42%", "The timeline is empty", "Not while recording".
    pub tooltip: String,
    /// The key caps of `export`.
    pub keys: Vec<Vec<String>>,
}

/// The header for `state`. `narrow`: the window is in the Narrow layout
/// (G§3.15, `LocalUi.layout`), which adds the layers button; nothing else
/// the header gets says so.
pub fn header(state: &State, facts: &GuiFacts, keymap: &Keymap, narrow: bool) -> HeaderView {
    let phase = &state.runtime.record;
    HeaderView {
        title: state.project.name.clone(),
        subtitle: subtitle(state),
        record: record_button(state, keys_of(keymap, "record.toggle")),
        export: export_slot(state, facts, keys_of(keymap, "export")),
        record_line: matches!(
            phase,
            RecordPhase::Starting { .. }
                | RecordPhase::Recording { .. }
                | RecordPhase::Finalizing { .. }
        ),
        layers_button: narrow,
    }
}

/// The caps of binding `id`'s first usable key, as one chord (empty when
/// it has none).
fn keys_of(keymap: &Keymap, id: &str) -> Vec<Vec<String>> {
    keymap
        .binding(id)
        .and_then(|b| {
            usable_keys(keymap, b)
                .first()
                .map(|k| KeyChord::cap_legends(k))
        })
        .into_iter()
        .collect()
}

/// "Recovering take 7…" while a recovery probe is out (D-47), else the
/// timeline's summary.
fn subtitle(state: &State) -> Subtitle {
    let recovering = &state.runtime.recovering;
    if let Some(first) = recovering.iter().next() {
        let text = if recovering.len() == 1 {
            format!("Recovering take {}\u{2026}", first.number())
        } else {
            format!("Recovering {} takes\u{2026}", recovering.len())
        };
        return Subtitle::Recovering { text };
    }
    match format::clips_summary(&state.project.timeline.clips, state.project.canvas.fps) {
        Some((count, duration)) => Subtitle::Clips { count, duration },
        None => Subtitle::NoClips,
    }
}

/// The record button for the phase (G§4.4's table). Idle, the first
/// blocker that applies wins: the engine is stuck, `qrec.json` is invalid,
/// the scene is empty, an export runs.
fn record_button(state: &State, keys: Vec<Vec<String>>) -> RecordButton {
    let runtime = &state.runtime;
    let (kind, enabled, tooltip) = match &runtime.record {
        RecordPhase::Idle => {
            let blocker = if runtime.engine_stuck.is_some() {
                Some("The capture engine is stuck (GPU driver?); restart Qrec")
            } else if runtime.disk_invalid.is_some() {
                Some("qrec.json on disk is invalid")
            } else if state.project.scene.is_empty() {
                Some("Add a layer or audio track first")
            } else if exporting(state) {
                Some("Wait for the export to finish")
            } else {
                None
            };
            match blocker {
                Some(why) => (RecordButtonKind::Record, false, why.to_owned()),
                None => (RecordButtonKind::Record, true, "Record".to_owned()),
            }
        }
        RecordPhase::Countdown { .. } => (
            RecordButtonKind::Cancel,
            true,
            "Cancel the countdown".to_owned(),
        ),
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            (RecordButtonKind::Stop, true, "Stop recording".to_owned())
        }
        RecordPhase::Finalizing { take, .. } => (
            RecordButtonKind::Saving,
            false,
            format!("Take {} is finalizing", take.number()),
        ),
    };
    RecordButton {
        kind,
        enabled,
        keys,
        tooltip,
    }
}

/// An export job runs (whoever started it).
fn exporting(state: &State) -> bool {
    state
        .runtime
        .jobs
        .values()
        .any(|job| matches!(job.kind, JobKind::Export { .. }))
}

/// The export slot: a running export's percent (a click toggles its
/// panel), else `Export…`, disabled while recording and on an empty
/// timeline.
fn export_slot(state: &State, facts: &GuiFacts, keys: Vec<Vec<String>>) -> ExportSlot {
    let pressed = facts.export_panel.is_some();
    let running = state
        .runtime
        .jobs
        .values()
        .find(|job| matches!(job.kind, JobKind::Export { .. }));
    if let Some(job) = running {
        // The percent doesn't depend on the clock (only the ETA does).
        let percent = export_readout(job, job.started).map_or(0, |r| r.percent);
        return ExportSlot {
            percent: Some(percent),
            enabled: true,
            pressed,
            tooltip: format!("Exporting {percent}%"),
            keys,
        };
    }
    let (enabled, tooltip) = if !state.runtime.record.is_idle() {
        (false, "Not while recording")
    } else if state.project.timeline.clips.is_empty() {
        (false, "The timeline is empty")
    } else {
        (true, "Export")
    };
    ExportSlot {
        percent: None,
        enabled,
        pressed,
        tooltip: tooltip.to_owned(),
        keys,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ids::{JobId, TakeId, TimerId};
    use crate::keymap;
    use crate::state::{Job, RecordMode};
    use crate::testkit;
    use crate::view::{ExportPanelKind, OverlayKind};

    /// The layers button is the narrow layout's, open sheet or not: the
    /// facts the header shares with the hint bar can't say the layout.
    #[test]
    fn the_layers_button_follows_the_layout() {
        let state = testkit::design_state();
        let keymap = keymap::resolve("");
        let mut facts = GuiFacts::default();
        assert!(header(&state, &facts, &keymap, true).layers_button);
        assert!(!header(&state, &facts, &keymap, false).layers_button);
        facts.top_overlay = Some(OverlayKind::LayersSheet);
        assert!(header(&state, &facts, &keymap, true).layers_button);
        assert!(!header(&state, &facts, &keymap, false).layers_button);
    }

    fn phase(state: &mut State, phase: &str) {
        let take = TakeId::numbered(10);
        state.runtime.record = match phase {
            "countdown" => RecordPhase::Countdown {
                remaining: 3,
                mode: RecordMode::Append,
                timer: TimerId(1),
                start_req: None,
                wait: false,
            },
            "starting" => RecordPhase::Starting {
                take,
                mode: RecordMode::Append,
                start_req: None,
                wait: false,
            },
            "recording" => RecordPhase::Recording {
                take,
                mode: RecordMode::Append,
                elapsed_ns: 161_400_000_000,
                waiters: vec![],
            },
            "finalizing" => RecordPhase::Finalizing {
                take,
                mode: RecordMode::Append,
                cancelled: false,
                stop_reqs: vec![],
                waiters: vec![],
                discard: None,
            },
            _ => RecordPhase::Idle,
        };
    }

    fn export_job(state: &mut State, done: u64) {
        state.runtime.jobs.insert(
            JobId(7),
            Job {
                id: JobId(7),
                kind: JobKind::Export {
                    out: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                    files: Default::default(),
                    total_frames: 7391,
                    seg_frames: vec![7391],
                    seg_done: vec![done],
                    seg_clips: Vec::new(),
                    stitch_total_ns: 1,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: state.project.created_at,
            },
        );
    }

    /// The header of every record phase and of the idle blockers (G§4.4).
    #[test]
    fn header_snapshot() {
        let keymap = keymap::resolve("");
        let base = testkit::design_state();
        let facts = GuiFacts::default();
        let mut cases: Vec<(&str, State, GuiFacts)> = Vec::new();
        cases.push(("idle (S02)", base.clone(), facts.clone()));
        for name in ["countdown", "starting", "recording", "finalizing"] {
            let mut state = base.clone();
            phase(&mut state, name);
            cases.push((name, state, facts.clone()));
        }
        let mut empty = testkit::state(crate::model::Project::new(
            "rust-traits-talk",
            base.project.created_at,
            &Default::default(),
        ));
        empty.ui.playhead = 0;
        cases.push(("first run (S01a)", empty, facts.clone()));
        let mut exporting = base.clone();
        export_job(&mut exporting, 3449);
        cases.push(("exporting (S15b)", exporting.clone(), facts.clone()));
        cases.push((
            "exporting, panel open",
            exporting,
            GuiFacts {
                export_panel: Some(ExportPanelKind::Running),
                ..facts.clone()
            },
        ));
        let mut stuck = base.clone();
        stuck.runtime.engine_stuck = Some("the capture engine wedged".into());
        cases.push(("engine stuck", stuck, facts.clone()));
        let mut invalid = base.clone();
        invalid.runtime.disk_invalid = Some("/timeline/clips/4/out: 1400 > 1377".into());
        cases.push(("qrec.json invalid", invalid, facts.clone()));
        let mut recovering = base.clone();
        recovering.runtime.recovering = [TakeId::numbered(7)].into();
        cases.push(("recovering", recovering, facts.clone()));
        let mut no_clips = base;
        no_clips.project.timeline.clips.clear();
        cases.push(("no clips (S01b)", no_clips, facts));
        let all: Vec<serde_json::Value> = cases
            .into_iter()
            .map(|(name, state, facts)| {
                json!({"case": name, "header": header(&state, &facts, &keymap, false)})
            })
            .collect();
        insta::assert_json_snapshot!(all);
    }

    #[test]
    fn the_record_button_follows_the_phase() {
        let keymap = keymap::resolve("");
        let facts = GuiFacts::default();
        let mut state = testkit::design_state();
        let view = header(&state, &facts, &keymap, false);
        assert_eq!(view.title, "rust-traits-talk");
        assert_eq!(
            view.subtitle,
            Subtitle::Clips {
                count: "10 clips \u{b7} ".into(),
                duration: "00:04:06:11".into()
            }
        );
        assert_eq!(
            view.record,
            RecordButton {
                kind: RecordButtonKind::Record,
                enabled: true,
                keys: vec![vec!["R".into()]],
                tooltip: "Record".into()
            }
        );
        assert!(!view.record_line);
        for (name, kind, enabled, line, tooltip) in [
            (
                "countdown",
                RecordButtonKind::Cancel,
                true,
                false,
                "Cancel the countdown",
            ),
            (
                "starting",
                RecordButtonKind::Stop,
                true,
                true,
                "Stop recording",
            ),
            (
                "recording",
                RecordButtonKind::Stop,
                true,
                true,
                "Stop recording",
            ),
            (
                "finalizing",
                RecordButtonKind::Saving,
                false,
                true,
                "Take 10 is finalizing",
            ),
        ] {
            phase(&mut state, name);
            let view = header(&state, &facts, &keymap, false);
            assert_eq!(view.record.kind, kind, "{name}");
            assert_eq!(view.record.enabled, enabled, "{name}");
            assert_eq!(view.record_line, line, "{name}");
            assert_eq!(view.record.tooltip, tooltip, "{name}");
            // Export… waits while recording.
            assert!(!view.export.enabled, "{name}");
            assert_eq!(view.export.tooltip, "Not while recording", "{name}");
        }
        // A rebound key shows on the button.
        let rebound = keymap::resolve("[bindings]\n\"record.toggle\" = \"F9\"\n");
        let view = header(&state, &facts, &rebound, false);
        assert_eq!(view.record.keys, vec![vec!["F9".to_owned()]]);
    }

    #[test]
    fn the_export_slot_shows_any_running_export() {
        let keymap = keymap::resolve("");
        let mut state = testkit::design_state();
        let idle = header(&state, &GuiFacts::default(), &keymap, false).export;
        assert_eq!(
            idle,
            ExportSlot {
                percent: None,
                enabled: true,
                pressed: false,
                tooltip: "Export".into(),
                keys: vec![vec!["Ctrl".into(), "E".into()]],
            }
        );
        export_job(&mut state, 3449);
        let running = header(&state, &GuiFacts::default(), &keymap, false);
        assert_eq!(running.export.percent, Some(42));
        assert_eq!(running.export.tooltip, "Exporting 42%");
        assert!(running.export.enabled && !running.export.pressed);
        // Record waits for the export.
        assert!(!running.record.enabled);
        assert_eq!(running.record.tooltip, "Wait for the export to finish");
        let open = GuiFacts {
            export_panel: Some(ExportPanelKind::Running),
            ..GuiFacts::default()
        };
        assert!(header(&state, &open, &keymap, false).export.pressed);
        // An empty timeline has nothing to export.
        let mut empty = testkit::design_state();
        empty.project.timeline.clips.clear();
        let view = header(&empty, &GuiFacts::default(), &keymap, false);
        assert!(!view.export.enabled);
        assert_eq!(view.export.tooltip, "The timeline is empty");
        assert_eq!(view.subtitle, Subtitle::NoClips);
    }

    #[test]
    fn several_recoveries_are_counted() {
        let keymap = keymap::resolve("");
        let mut state = testkit::design_state();
        state.runtime.recovering = [TakeId::numbered(7), TakeId::numbered(9)].into();
        let view = header(&state, &GuiFacts::default(), &keymap, false);
        assert_eq!(
            view.subtitle,
            Subtitle::Recovering {
                text: "Recovering 2 takes\u{2026}".into()
            }
        );
    }
}
