//! Capability detection (A§3 `Capabilities`, A§8.10): the ffmpeg version,
//! resolved path and encoders; the effective VA render node and vendor, the
//! h264_vaapi export check (a child process, 10 s) and the matching
//! `va*h264enc` factory; cached in `$XDG_CACHE_HOME/qrec`. Sent as
//! `Event::Capabilities` before the first dispatch. Owner: session. Empty stub.
