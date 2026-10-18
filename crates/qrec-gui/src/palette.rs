//! The command palette (S19, S19b, C34; gated on G§0.2's frames). Owner:
//! WP9. It re-registers `palette.open` once its frame is drawn into the
//! build.
//!
//! Until then, `Ctrl+K`, `:` and the header's Commands button show the
//! info toast "Not built yet" (G§1.1, registered by the app). What the
//! palette shows is `qrec_core::view::palette` (rows, matches, Recent,
//! groups); what a row does is [`decide_row`]: what its key does, through
//! the key router's own path (`router::decide_button`, which
//! `KeyRouter::run_binding` carries out), so a row runs its command after
//! the key's pre-steps, opens what the key opens, or says why it can't.
//! "Clear undo history" (D-19) sends `HistoryClear`; the core's
//! `confirmation_required` opens the modal by the default reply policy,
//! whose Clear history sends it again confirmed (`Store::dispatch_confirmed`).

use gtk::prelude::*;

use crate::Ctx;
use crate::keys::router::{self, KeyDecision, RouterState};

/// Whether the palette is built (`GuiFacts.built`). WP9 sets it.
pub const BUILT: bool = false;

/// What running palette row `id` does: what its key does (the router's
/// `decide_button`, a button's path to a binding, G§3.7). The palette
/// closes, then `KeyRouter::run_binding(id)` carries it out.
pub fn decide_row(id: &str, rs: &RouterState<'_>) -> KeyDecision {
    router::decide_button(id, rs)
}

/// The palette (opened in `Layer::Palette`).
pub struct Palette {
    root: gtk::Box,
}

impl Palette {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("palette");
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use qrec_core::history::HistoryEntry;
    use qrec_core::keymap::{self, Action, Keymap, action_for};
    use qrec_core::state::{CanvasMode, RecordMode, RecordPhase};
    use qrec_core::view::confirm::{self, ConfirmFacts};
    use qrec_core::view::notices::Notice;
    use qrec_core::view::{FocusRegion, palette};
    use qrec_core::{ClipId, Command, ErrorCode, Frame, LayerId, State, TakeId, testkit};

    use super::*;
    use crate::host::queue::Queue;
    use crate::keys::router::{KeySink, apply};
    use crate::store::Store;
    use crate::store::local::{LocalUi, Overlay, StartupPhase};

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Dispatch(Command),
        Gui(String),
        Other,
    }

    #[derive(Default)]
    struct Recorder(Vec<Call>);

    impl KeySink for Recorder {
        fn dispatch(&mut self, cmd: Command) {
            self.0.push(Call::Dispatch(cmd));
        }
        fn gui(&mut self, id: &str) {
            self.0.push(Call::Gui(id.to_owned()));
        }
        fn notice(&mut self, _: Notice) {
            self.0.push(Call::Other);
        }
        fn set_playing_rate(&mut self, _: f64) {
            self.0.push(Call::Other);
        }
        fn player_rate(&mut self, _: f64) {
            self.0.push(Call::Other);
        }
        fn key_parks(&mut self, _: Frame) {
            self.0.push(Call::Other);
        }
        fn cancel_drag(&mut self) {
            self.0.push(Call::Other);
        }
        fn close_overlay(&mut self) {
            self.0.push(Call::Other);
        }
    }

    /// The design state with 128 undo steps.
    fn with_history(mut state: State) -> State {
        state.history.entries = Arc::new(
            (1..=128)
                .map(|seq| HistoryEntry {
                    seq,
                    ts: state.project.created_at,
                    label: format!("Edit {seq}"),
                    cmd: serde_json::Value::Null,
                    forward: Default::default(),
                    inverse: Default::default(),
                    after_hash: String::new(),
                })
                .collect(),
        );
        state.history.cursor = 128;
        state.history.next_seq = 129;
        state
    }

    /// The window states the table runs in, open: S02, S06, the canvas
    /// with L2 selected, a take recording.
    fn scenes() -> Vec<(&'static str, State, LocalUi)> {
        let local = LocalUi {
            startup: StartupPhase::Open,
            focus: FocusRegion::Timeline,
            ..LocalUi::default()
        };
        let s02 = with_history(testkit::design_state());
        let mut s06 = s02.clone();
        s06.ui.canvas_mode = CanvasMode::Program;
        s06.ui.selection = vec![ClipId::from_raw("c2d0")];
        let mut canvas = s02.clone();
        canvas.ui.selected_layer = Some(LayerId::numbered(2));
        let mut recording = s06.clone();
        recording.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        vec![
            ("S02", s02, local.clone()),
            ("S06", s06, local.clone()),
            (
                "canvas",
                canvas,
                LocalUi {
                    focus: FocusRegion::Stage,
                    ..local.clone()
                },
            ),
            ("recording", recording, local),
        ]
    }

    /// The GUI action a row of `id` opens when its key does (G§3.8 step
    /// 10), else `id` itself.
    fn window_action(id: &str) -> &str {
        match id {
            "export" => "export.panel",
            "autocut.preview" => "autocut.page",
            other => other,
        }
    }

    /// The dispatch table (WP9): every row the palette offers runs, as its
    /// key would: a row with a command dispatches `action_for`'s command
    /// after the key's pre-steps (none here: nothing plays), a row the
    /// window handles runs its GUI action; none is refused or ignored.
    #[test]
    fn every_enabled_row_runs_as_its_key_would() {
        let keymap: Keymap = keymap::resolve("");
        let mut ran = (0, 0);
        for (name, state, local) in scenes() {
            let rs = RouterState {
                state: &state,
                local: &local,
                keymap: &keymap,
                player_frame: None,
            };
            let view = palette::palette(&keymap, &state, "", &[]);
            for row in view
                .groups
                .iter()
                .flat_map(|g| &g.rows)
                .filter(|r| r.enabled)
            {
                let decision = decide_row(&row.id, &rs);
                let mut sink = Recorder::default();
                assert!(
                    apply(&decision, &state, &mut sink),
                    "{name}: {} unused",
                    row.id
                );
                match &decision {
                    KeyDecision::Run { cmd, pre, .. } => {
                        assert_eq!(
                            action_for(&row.id, &state),
                            Some(Action::Run(cmd.clone())),
                            "{name}: {}",
                            row.id
                        );
                        assert!(
                            pre.is_empty()
                                || row.id.starts_with("record.")
                                || row.id.starts_with("shuttle."),
                            "{name}: {} {pre:?}",
                            row.id
                        );
                        assert_eq!(
                            sink.0.last(),
                            Some(&Call::Dispatch(cmd.clone())),
                            "{name}: {}",
                            row.id
                        );
                        ran.0 += 1;
                    }
                    KeyDecision::Gui { id } => {
                        assert_eq!(id, window_action(&row.id), "{name}: {}", row.id);
                        assert_eq!(sink.0, [Call::Gui(id.clone())], "{name}: {}", row.id);
                        ran.1 += 1;
                    }
                    other => panic!("{name}: the enabled row {} gives {other:?}", row.id),
                }
            }
        }
        assert!(ran.0 > 60 && ran.1 > 40, "{ran:?}");
    }

    /// "Clear undo history" → the confirmation modal → `HistoryClear`
    /// sent again, confirmed: the palette's path, the default reply policy
    /// on the core's own answer, the modal's copy, its Clear history.
    #[test]
    fn clear_undo_history_asks_then_sends_it_confirmed() {
        let (_, state, local) = scenes().remove(1);
        let keymap = keymap::resolve("");
        let rs = RouterState {
            state: &state,
            local: &local,
            keymap: &keymap,
            player_frame: None,
        };
        let row = &palette::palette(&keymap, &state, "clear", &[]).groups[0].rows[0];
        assert_eq!((row.id.as_str(), row.enabled), ("history.clear", true));
        let decision = decide_row(&row.id, &rs);
        assert_eq!(
            decision,
            KeyDecision::Run {
                id: "history.clear".into(),
                pre: vec![],
                cmd: Command::HistoryClear,
            }
        );

        let store = Store::new(
            Queue::new(|| {}),
            PathBuf::from("/projects/rust-traits-talk"),
            Arc::new(state.clone()),
            keymap.clone(),
            local.clone(),
        );
        // What `KeyRouter::run_binding` sends: the command, by `dispatch`.
        store.dispatch(Command::HistoryClear);
        let _ = store.queue().take_all(); // no host: its "busy" is dropped
        let ctx = testkit::ctx(testkit::CREATED_AT, 7);
        let asked = qrec_core::apply(&state, &ctx, Command::HistoryClear).unwrap_err();
        assert_eq!(asked.code, ErrorCode::ConfirmationRequired);
        store.on_reply(1, Err(asked.clone()), false);
        let overlay = Overlay::Confirm {
            cmd: Command::HistoryClear,
            error: asked.clone(),
        };
        assert_eq!(store.local().overlays, std::slice::from_ref(&overlay));
        // The modal shows `view::confirm`'s copy (`modal::content_for`).
        let content = confirm::modal(
            &Command::HistoryClear,
            &asked,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.title, "Get out of my swamp?");
        assert_eq!(content.lead, "Clear undo history?");
        assert!(
            content.body.as_deref().is_some_and(
                |b| b.starts_with("This removes 128 undo steps from .qrec/history.jsonl.")
            ),
            "{content:?}"
        );
        assert_eq!(content.confirm, "Clear history");
        // Its Clear history: the command again, confirmed, then it closes.
        store.dispatch_confirmed(Command::HistoryClear);
        store.update_local(move |l| {
            l.close_overlays(|o| *o == overlay);
        });
        assert_eq!(store.pending_requests(), 1, "sent again");
        assert!(store.local().overlays.is_empty());
        // The core clears it once confirmed.
        let confirmed = qrec_core::Ctx {
            confirmed: true,
            ..ctx
        };
        let cleared = qrec_core::apply(&state, &confirmed, Command::HistoryClear).unwrap();
        assert!(cleared.state.history.entries.is_empty());
        let rs = RouterState {
            state: &cleared.state,
            ..rs
        };
        assert_eq!(
            decide_row("history.clear", &rs),
            KeyDecision::Unavailable {
                id: "history.clear".into(),
                reason: "The undo history is empty".into(),
            }
        );
    }
}
