//! Level meters without a capture pipeline (GUI D-03, docs/GUI.md G§3.9):
//! while the GUI shows Program mode and nothing records, the live pipeline
//! is down, yet the Onion layers panel keeps its meters live. One pipeline
//! per device, `pulsesrc device=X ! audioconvert ! level interval=50ms !
//! fakesink sync=false` (the fake backend: the engine's tone with
//! silences), on thread `qrec-meters`; readings go out as
//! [`SideEvent::Level`], like the engine's, straight from the bus's sync
//! handler. PipeWire lets a second stream open the same source; a device
//! that can't be opened (or fails later) is logged and its meter stays
//! idle, while the others go on.
//!
//! [`MeterMonitor::stop`] returns at once: no reading goes out after it,
//! and the pipelines reach NULL within 5 s ([`MeterMonitor::is_finished`]
//! says when, for a GUI that keeps its main loop running until then, R4).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use gst::prelude::*;

use super::fake::{fake_tone, is_fake_audio};
use super::live::{fake_audio_source, level_reading};
use super::util::{capsfilter, error_text, make, set_null_with_deadline, set_value};
use super::{Backend, SideEvent, TEARDOWN_TIMEOUT};
use crate::gst_init;

/// How often a meter reports its device's level (`level interval`).
pub const METER_INTERVAL: Duration = Duration::from_millis(50);

/// The meter pipelines of a set of audio devices.
#[derive(Debug)]
pub struct MeterMonitor {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    /// [`MeterMonitor::stop`] was called: no reading goes out any more.
    stopping: AtomicBool,
    /// Every pipeline reached NULL (or was left behind after 5 s).
    finished: AtomicBool,
}

#[derive(Debug)]
enum Msg {
    Stop,
    /// The meter of this device (by position) posted an error.
    Failed(usize, String),
}

impl MeterMonitor {
    /// Starts one meter per device (`AudioTrack.source` names; repeats are
    /// metered once), sending `SideEvent::Level` readings about every 50 ms
    /// to `side`. GStreamer is initialized on the monitor's thread.
    pub fn start(
        devices: Vec<String>,
        backend: Backend,
        side: Sender<SideEvent>,
    ) -> std::io::Result<MeterMonitor> {
        let mut unique: Vec<String> = Vec::with_capacity(devices.len());
        for device in devices {
            if !unique.contains(&device) {
                unique.push(device);
            }
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared::default());
        if unique.is_empty() {
            shared.finished.store(true, Ordering::SeqCst);
            return Ok(MeterMonitor { tx, shared });
        }
        let worker = Worker {
            devices: unique,
            backend,
            side,
            tx: tx.clone(),
            shared: Arc::clone(&shared),
        };
        thread::Builder::new()
            .name("qrec-meters".into())
            .spawn(move || worker.run(&rx))?;
        Ok(MeterMonitor { tx, shared })
    }

    /// Stops every meter. Asynchronous: nothing is sent after it returns,
    /// and the pipelines reach NULL within 5 s. Later calls do nothing.
    pub fn stop(&self) {
        if !self.shared.stopping.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Msg::Stop);
        }
    }

    /// Whether every pipeline reached NULL after [`stop`](Self::stop) (or
    /// was abandoned to a wedged driver after 5 s).
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::SeqCst)
    }
}

impl Drop for MeterMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    devices: Vec<String>,
    backend: Backend,
    side: Sender<SideEvent>,
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

impl Worker {
    fn run(self, rx: &Receiver<Msg>) {
        let mut meters: Vec<Option<gst::Pipeline>> = Vec::new();
        let stopping = || self.shared.stopping.load(Ordering::SeqCst);
        if !stopping() {
            match gst_init::init() {
                Ok(()) => {
                    for (index, device) in self.devices.iter().enumerate() {
                        // A stop that came meanwhile opens nothing more.
                        let meter = if stopping() {
                            None
                        } else {
                            self.open(index, device)
                        };
                        meters.push(meter);
                    }
                }
                Err(e) => tracing::warn!("meters: {e}; the meters stay idle"),
            }
        }
        tracing::debug!(
            "meters: {} of {} device(s) metered",
            meters.iter().flatten().count(),
            self.devices.len()
        );
        // The handle's `stop` (also from its `Drop`) always comes.
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Stop => break,
                Msg::Failed(index, message) => {
                    let device = self.devices.get(index).map_or("?", String::as_str);
                    tracing::warn!("meters: {device}: {message}; its meter stays idle");
                    if let Some(pipeline) = meters.get_mut(index).and_then(Option::take) {
                        stop_pipelines(vec![pipeline]);
                    }
                }
            }
        }
        stop_pipelines(meters.into_iter().flatten().collect());
        self.shared.finished.store(true, Ordering::SeqCst);
    }

    /// The meter of `device`, playing; `None` (logged) when it can't be.
    fn open(&self, index: usize, device: &str) -> Option<gst::Pipeline> {
        let pipeline = match self.build(index, device) {
            Ok(pipeline) => pipeline,
            Err(e) => {
                tracing::warn!("meters: {device}: {e}; its meter stays idle");
                return None;
            }
        };
        if pipeline.set_state(gst::State::Playing).is_err() {
            tracing::warn!("meters: {device} can't be opened; its meter stays idle");
            stop_pipelines(vec![pipeline]);
            return None;
        }
        Some(pipeline)
    }

    /// `<source> ! audioconvert ! level interval=50ms ! fakesink sync=false`.
    fn build(&self, index: usize, device: &str) -> Result<gst::Pipeline, String> {
        let pipeline = gst::Pipeline::with_name(&format!("qrec-meter-{index}"));
        let mut chain: Vec<gst::Element> = Vec::new();
        match self.backend {
            Backend::Fake => {
                if !is_fake_audio(device) {
                    return Err(format!("there is no fake audio source {device}"));
                }
                let tone = fake_tone(device);
                chain.push(fake_audio_source("m-src", &tone)?);
                chain.push(capsfilter(
                    "m-caps",
                    &format!("audio/x-raw,rate=48000,channels={}", tone.channels),
                )?);
            }
            Backend::Real => {
                let src = make("pulsesrc", "m-src")?;
                set_value(&src, "device", device)?;
                set_value(&src, "client-name", "Qrec")?;
                chain.push(src);
            }
        }
        chain.push(make("audioconvert", "m-convert")?);
        let level = make("level", "m-level")?;
        set_value(&level, "interval", METER_INTERVAL.as_nanos() as u64)?;
        set_value(&level, "post-messages", true)?;
        chain.push(level);
        let sink = make("fakesink", "m-sink")?;
        set_value(&sink, "sync", false)?;
        chain.push(sink);
        pipeline
            .add_many(chain.iter())
            .map_err(|e| format!("can't build the meter: {e}"))?;
        gst::Element::link_many(chain.iter()).map_err(|e| format!("can't link the meter: {e}"))?;

        let bus = pipeline.bus().ok_or("the meter has no bus")?;
        let (side, tx, shared) = (self.side.clone(), self.tx.clone(), Arc::clone(&self.shared));
        let device = device.to_owned();
        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Element(e) => {
                    if let Some(s) = e.structure()
                        && s.name() == "level"
                        && !shared.stopping.load(Ordering::SeqCst)
                        && let Some(reading) = level_reading(&device, s)
                    {
                        let _ = side.send(SideEvent::Level(reading));
                    }
                }
                gst::MessageView::Error(err) => {
                    let (_, detailed) = error_text(&err.error(), err.debug().as_deref());
                    let _ = tx.send(Msg::Failed(index, detailed));
                }
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
        Ok(pipeline)
    }
}

/// NULL within [`TEARDOWN_TIMEOUT`], or left behind with a warning.
fn stop_pipelines(pipelines: Vec<gst::Pipeline>) {
    if pipelines.is_empty() {
        return;
    }
    let elements = pipelines.into_iter().map(|p| p.upcast()).collect();
    if !set_null_with_deadline(elements, TEARDOWN_TIMEOUT) {
        tracing::warn!(
            "meters: a meter didn't stop within {} s; left behind",
            TEARDOWN_TIMEOUT.as_secs()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_devices_start_nothing() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let monitor = MeterMonitor::start(Vec::new(), Backend::Fake, tx).expect("starts");
        assert!(monitor.is_finished());
        monitor.stop();
        assert!(rx.try_recv().is_err(), "no readings");
    }
}
