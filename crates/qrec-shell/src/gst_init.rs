//! `gst::init()` on a helper thread with a 30 s deadline: a hung GPU driver
//! can block the plugin scan (A§8.10). Never for ffmpeg-only commands, never
//! on the GTK main thread. Owner: capture. Empty stub (feature `gst`).
