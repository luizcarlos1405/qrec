//! Source discovery (A§8.12, `SourceInventory`): screens from `wl_output`
//! v4 (fallback `niri msg --json outputs`), cameras from GStreamer's
//! DeviceMonitor, mics (`alsa_input.*`) and monitors (`*.monitor`) from
//! `pactl --format=json`; the fake inventory under
//! `QREC_CAPTURE_BACKEND=fake`. Owner: media. Empty stub.
