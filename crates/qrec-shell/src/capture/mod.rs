//! The capture engine (A§8.11, `CaptureEngine`): the live pipeline, record
//! bins attached at T0 (buffer probes, `videorate skip-to-first`, x264/VA
//! encoders, matroskamux), first-buffer and health checks, EOS/teardown
//! timeouts (`EngineWedged`), the ScreenCast portal (ashpd under
//! `futures_lite::block_on`, restore tokens) and the fake backend
//! (`QREC_CAPTURE_BACKEND=fake`). All GStreamer calls happen on one engine
//! thread; GStreamer code goes behind the `gst` feature. Owner: capture.
//! Empty stub.
