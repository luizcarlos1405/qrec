//! The process runner (A§8.7): stdin null, `process_group(0)`, stdout piped
//! only when parsed (drained on its own thread), stderr appended to a log
//! file; timeouts (ffprobe 20 s, pactl 3 s, VA/encoder probes 10 s,
//! `wl-copy` 5 s), the 60 s ffmpeg stall watchdog, and SIGTERM → SIGKILL
//! after 2 s → reaper thread (the session thread never blocks in `wait()`).
//! Logs: `.qrec/logs/YYYYMMDD-HHMMSS-<pid>-j<job>-<step>.log`.
//! Owner: session. Empty stub.
