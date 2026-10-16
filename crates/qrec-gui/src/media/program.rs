//! Program playback sync (G§3.10): a pure state machine plus thin glue
//! between the snapshots (`runtime.playing`, `ui.playhead`, the plan) and
//! the `ProgramPlayer` (load, seek, play with a rate, pause, scrub), the
//! trim scrub (`LocalUi.trim_frame`) and the bin preview
//! (`LocalUi.take_preview`). Owner: WP4. Stub until WP4 lands: nothing
//! plays. The `ProgramPlayer` itself is built (WP1 part 1) and
//! `MediaHub::on_opened` spawns it, but nothing loads a plan into it yet.
