//! Level meters without a capture pipeline (GUI D-03, docs/GUI.md G§3.9):
//! while the GUI shows Program mode and nothing records, the live pipeline
//! is down, yet the Onion layers panel keeps its meters live. One pipeline
//! per device, `pulsesrc device=X ! audioconvert ! level interval=50ms !
//! fakesink sync=false` (the fake backend: the engine's tone with
//! silences), on thread `qrec-meters`; readings go out as
//! [`SideEvent::Level`], like the engine's. PipeWire lets a second stream
//! open the same source.
//!
//! Not built yet: a monitor sends no readings (idle meters, as for a
//! device that refuses a second stream).

use crossbeam_channel::Sender;

use super::{Backend, SideEvent};

/// The meter pipelines of a set of audio devices.
#[derive(Debug)]
pub struct MeterMonitor {
    _private: (),
}

impl MeterMonitor {
    /// Starts one meter per device (`AudioTrack.source` names), sending
    /// `SideEvent::Level` readings about every 50 ms to `side`.
    pub fn start(
        devices: Vec<String>,
        backend: Backend,
        side: Sender<SideEvent>,
    ) -> std::io::Result<MeterMonitor> {
        let _ = (backend, side);
        tracing::debug!(
            "meters: the meter monitor isn't built yet; {} device(s) stay idle",
            devices.len()
        );
        Ok(MeterMonitor { _private: () })
    }

    /// Stops every pipeline. Asynchronous: they reach NULL within 5 s.
    pub fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unbuilt_monitor_starts_and_stops_quietly() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let monitor =
            MeterMonitor::start(vec!["alsa_input.fake".into()], Backend::Fake, tx).expect("starts");
        monitor.stop();
        assert!(rx.try_recv().is_err(), "no readings");
    }
}
