//! The project store (A§7.6, A§7.7). Owner: store. Empty stub.
//!
//! Reads and writes `qrec.json` (tmp → fsync → rename → fsync(dir)),
//! `.qrec/ui.json` (coalesced, no fsync), `.qrec/history.jsonl` (one line
//! per `Append` via `O_APPEND` + `fdatasync`; tmp + rename for redo-tail
//! drops and `Rewrite`), `history.base.json` and the commit record
//! `history.head.json` (`{cursor, baseSeq, nextSeq, projectSha256, project}`).
//! On open: drop history lines with `seq ≥ nextSeq`, drop a torn last line,
//! detect external edits by hash, recovery mode, migration backups
//! (`.qrec/qrec.v{N}.json.bak`, never overwritten) and rejected files
//! (`.qrec/rejected/`).
