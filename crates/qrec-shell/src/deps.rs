//! What a `Session` is built from (A§8.1, A§8.11, foundation): the capture
//! engine, the media runner, the clock and the entropy source. Fake `Deps`
//! make the Session testable without GStreamer or ffmpeg.

use chrono::{DateTime, Local, SubsecRound, Utc};
use qrec_core::plans::{CapturePlan, ExportPlan, FramePlan, LiveScene};
use qrec_core::protocol::{ProbeJob, ProbePurpose};
use qrec_core::state::{HostKind, SilenceKey};
use qrec_core::{Event, JobId, TakeId};

use crate::msg::Inbox;

/// The Session's collaborators.
pub struct Deps {
    /// Real (GStreamer), fake (`QREC_CAPTURE_BACKEND=fake`) or [`NoCapture`].
    pub capture: Box<dyn CaptureEngine>,
    pub media: Box<dyn MediaRunner>,
    /// Now (whole seconds are taken by the Session) and the local UTC offset in seconds.
    pub clock: Box<dyn FnMut() -> (DateTime<Utc>, i32) + Send>,
    /// Fresh entropy per dispatch (`Ctx::entropy`).
    pub entropy: Box<dyn FnMut() -> u64 + Send>,
}

/// How `Session::open` opens a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOpts {
    pub host: HostKind,
    /// The caller will change the project: take the lock, recover takes.
    pub mutating: bool,
    /// Load `history.head.json` + history and never load an unusable
    /// `qrec.json`; nothing is written until a request saves (`qrec
    /// history`, `qrec history restore`, A§7.7).
    pub recovery: bool,
}

/// Runs ffprobe/ffmpeg jobs and take-directory deletion on worker threads;
/// results come back as events through `out` (A§8.7).
pub trait MediaRunner: Send {
    /// → `TakeProbed`.
    fn probe(&mut self, take: TakeId, purpose: ProbePurpose, jobs: Vec<ProbeJob>, out: Inbox);
    /// → `SilenceDetected` | `SilenceFailed`.
    fn detect_silence(&mut self, job: JobId, key: SilenceKey, argv: Vec<String>, out: Inbox);
    /// → `ExportProgress`… then `ExportFinished` | `ExportFailed` | `ExportCancelled`.
    fn render_export(&mut self, job: JobId, plan: ExportPlan, out: Inbox);
    /// → `FrameSaved` | `FrameFailed`.
    fn save_frame(&mut self, job: JobId, plan: FramePlan, out: Inbox);
    /// → `TakeDirsDeleted`.
    fn delete_take_dirs(&mut self, job: JobId, takes: Vec<TakeId>, out: Inbox);
    /// SIGTERM to the job's process group, SIGKILL after 2 s.
    fn cancel(&mut self, job: JobId);
}

/// The capture engine (A§8.11). All GStreamer calls happen on one engine thread.
pub trait CaptureEngine: Send {
    /// Builds or updates the live pipeline; idempotent. `None` stops it.
    fn set_live(&mut self, live: Option<LiveScene>, out: Inbox);
    /// Attaches the record bins at T0 → `CaptureStarted` | `CaptureFailed`.
    fn start(&mut self, plan: CapturePlan, out: Inbox);
    /// EOS to the record bins, then detach → `CaptureStopped`.
    fn stop(&mut self, take: &TakeId, cancel: bool);
    fn shutdown(&mut self);
}

/// A capture engine for hosts that can't record: every start fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCapture;

impl CaptureEngine for NoCapture {
    fn set_live(&mut self, _live: Option<LiveScene>, _out: Inbox) {}

    fn start(&mut self, plan: CapturePlan, out: Inbox) {
        out.event(Event::CaptureFailed {
            take: Some(plan.take),
            source: None,
            message: "capture is not available in this build or host".to_owned(),
        });
    }

    fn stop(&mut self, _take: &TakeId, _cancel: bool) {}

    fn shutdown(&mut self) {}
}

/// The system clock: now (truncated to whole seconds) and the local UTC offset.
pub fn system_clock() -> (DateTime<Utc>, i32) {
    let now = Local::now();
    (
        now.with_timezone(&Utc).trunc_subsecs(0),
        now.offset().local_minus_utc(),
    )
}

/// 64 bits from the OS random source.
pub fn os_entropy() -> u64 {
    getrandom::u64().expect("the OS random source is available")
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::ids::TakeId;

    #[test]
    fn no_capture_fails_starts() {
        let (inbox, rx) = Inbox::channel();
        let plan = CapturePlan {
            take: TakeId::numbered(1),
            dir: "takes/take-0001".into(),
            log: "takes/take-0001/recorder.log".into(),
            fps: qrec_core::Fps::new(30, 1),
            health_check_ms: 500,
            first_buffer_ms: 3000,
            eos_timeout_ms: 10_000,
            video: vec![],
            audio: vec![],
        };
        NoCapture.start(plan, inbox);
        match rx.recv().unwrap() {
            crate::msg::Msg::Event(Event::CaptureFailed { take, .. }) => {
                assert_eq!(take, Some(TakeId::numbered(1)))
            }
            other => panic!("{other:?}"),
        }
        let (now, _offset) = system_clock();
        assert_eq!(now.timestamp_subsec_nanos(), 0);
        assert_ne!(os_entropy(), os_entropy());
    }
}
