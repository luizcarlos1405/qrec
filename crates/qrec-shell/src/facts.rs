//! Gathers `Facts` for a `FactsNeeded` before `apply`/`on_event` (A§4.2):
//! folder name, audio devices (`pactl`), screens and cameras (`sources`),
//! output path checks, take files and dirs, file sizes and mtimes. Under
//! `QREC_CAPTURE_BACKEND=fake` device facts come from the fake inventory.
//! Owner: session. Empty stub.
