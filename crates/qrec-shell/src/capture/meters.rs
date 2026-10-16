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
//! says when). Each meter opens on a thread of its own (`qrec-meter-open`):
//! for `pulsesrc`, NULL → READY connects to the sound server and waits for
//! it (libpulse gives up only after about 30 s), so a stuck server never
//! delays the other meters or the stop. A meter still opening at the stop
//! is left behind and goes back to NULL by itself once its open returns.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

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
    /// The meter of this device finished opening: playing, or `None` when
    /// it couldn't be opened or the monitor stopped meanwhile (it is back
    /// at NULL then).
    Opened(usize, Option<gst::Pipeline>),
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
        let count = self.devices.len();
        let mut meters: Vec<Option<gst::Pipeline>> = (0..count).map(|_| None).collect();
        let mut failed = vec![false; count];
        let mut opening = 0usize;
        let stopping = || self.shared.stopping.load(Ordering::SeqCst);
        if !stopping() {
            match gst_init::init() {
                Ok(()) => {
                    for (index, device) in self.devices.iter().enumerate() {
                        // A stop that came meanwhile opens nothing more.
                        if !stopping() && self.open(index, device) {
                            opening += 1;
                        }
                    }
                }
                Err(e) => tracing::warn!("meters: {e}; the meters stay idle"),
            }
        }
        // The handle's `stop` (also from its `Drop`) always comes; after it,
        // the meters still opening get until the deadline to come back.
        let mut deadline: Option<Instant> = None;
        loop {
            let msg = match deadline {
                None => rx.recv().ok(),
                Some(at) => rx.recv_deadline(at).ok(),
            };
            let Some(msg) = msg else { break };
            match msg {
                Msg::Stop if deadline.is_none() => {
                    deadline = Some(Instant::now() + TEARDOWN_TIMEOUT);
                    stop_pipelines(meters.iter_mut().filter_map(Option::take).collect());
                }
                Msg::Stop => {}
                Msg::Opened(index, pipeline) => {
                    opening = opening.saturating_sub(1);
                    if let Some(pipeline) = pipeline {
                        if deadline.is_some() || failed.get(index).copied().unwrap_or(true) {
                            stop_pipelines(vec![pipeline]);
                        } else {
                            meters[index] = Some(pipeline);
                        }
                    }
                }
                Msg::Failed(index, message) => {
                    let device = self.devices.get(index).map_or("?", String::as_str);
                    tracing::warn!("meters: {device}: {message}; its meter stays idle");
                    if let Some(failed) = failed.get_mut(index) {
                        *failed = true;
                    }
                    if let Some(pipeline) = meters.get_mut(index).and_then(Option::take) {
                        stop_pipelines(vec![pipeline]);
                    }
                }
            }
            if deadline.is_some() && opening == 0 {
                break;
            }
        }
        if opening > 0 {
            tracing::warn!(
                "meters: {opening} meter(s) still opening at the stop; left behind (each goes \
                 back to NULL once its open returns)"
            );
        }
        self.shared.finished.store(true, Ordering::SeqCst);
    }

    /// Builds the meter of `device` and opens it on a thread of its own,
    /// which answers [`Msg::Opened`]; false (logged) when it can't even be
    /// built or the thread can't start.
    fn open(&self, index: usize, device: &str) -> bool {
        let pipeline = match self.build(index, device) {
            Ok(pipeline) => pipeline,
            Err(e) => {
                tracing::warn!("meters: {device}: {e}; its meter stays idle");
                return false;
            }
        };
        let (tx, shared, name) = (self.tx.clone(), Arc::clone(&self.shared), device.to_owned());
        let opening = pipeline.clone();
        let spawned = thread::Builder::new()
            .name("qrec-meter-open".into())
            .spawn(move || {
                let opened = opening.set_state(gst::State::Playing).is_ok();
                if !opened {
                    tracing::warn!("meters: {name} can't be opened; its meter stays idle");
                }
                // Stopped meanwhile: nothing else owns it any more.
                if !opened || shared.stopping.load(Ordering::SeqCst) {
                    stop_pipelines(vec![opening]);
                    let _ = tx.send(Msg::Opened(index, None));
                } else {
                    tracing::debug!("meters: {name} metered");
                    let _ = tx.send(Msg::Opened(index, Some(opening)));
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("meters: {device}: can't start its thread ({e}); its meter stays idle");
            stop_pipelines(vec![pipeline]);
            return false;
        }
        true
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
                #[cfg(test)]
                if let Some(hang) = tests::hang_for(device) {
                    pipeline
                        .add(&hang)
                        .map_err(|e| format!("can't build the meter: {e}"))?;
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
    use std::sync::{Condvar, Mutex};

    use gst::glib;
    use gst::subclass::prelude::*;

    use super::*;
    use crate::capture::fake::{FAKE_MIC, FAKE_MONITOR};
    use crate::gst_init::testing::serial;

    /// How a hanging open goes: held until released, then whether its
    /// pipeline went back to NULL.
    #[derive(Debug, Default)]
    pub(super) struct Gate {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    impl Gate {
        fn release(&self) {
            self.state.lock().unwrap().0 = true;
            self.changed.notify_all();
        }

        fn nulled(&self) -> bool {
            self.state.lock().unwrap().1
        }
    }

    /// The next fake meter of this device hangs while it opens.
    static HANG_NEXT: Mutex<Option<(String, Arc<Gate>)>> = Mutex::new(None);

    /// The padless element that makes a meter's open hang, when one is set
    /// up for `device` ([`HANG_NEXT`]).
    pub(super) fn hang_for(device: &str) -> Option<gst::Element> {
        let mut next = HANG_NEXT.lock().unwrap();
        if next.as_ref().is_none_or(|(d, _)| d != device) {
            return None;
        }
        let (_, gate) = next.take()?;
        let hang: Hang = glib::Object::new();
        *hang.imp().gate.lock().unwrap() = Some(gate);
        Some(hang.upcast())
    }

    mod imp {
        use super::*;

        /// Hangs in NULL → READY until its gate is released (like
        /// `pulsesrc` waiting for a stuck sound server).
        #[derive(Default)]
        pub struct Hang {
            pub(super) gate: Mutex<Option<Arc<Gate>>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Hang {
            const NAME: &'static str = "QrecTestMeterHang";
            type Type = super::Hang;
            type ParentType = gst::Element;
        }

        impl ObjectImpl for Hang {}

        impl GstObjectImpl for Hang {}

        impl ElementImpl for Hang {
            fn change_state(
                &self,
                transition: gst::StateChange,
            ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
                let gate = self.gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    let mut state = gate.state.lock().unwrap();
                    match transition {
                        gst::StateChange::NullToReady => {
                            while !state.0 {
                                state = gate.changed.wait(state).unwrap();
                            }
                        }
                        gst::StateChange::ReadyToNull => state.1 = true,
                        _ => {}
                    }
                }
                self.parent_change_state(transition)
            }
        }
    }

    glib::wrapper! {
        pub struct Hang(ObjectSubclass<imp::Hang>) @extends gst::Element, gst::Object;
    }

    /// A device whose meter hangs while it opens (a stuck sound server)
    /// keeps neither the other meters nor the stop waiting: they read and
    /// stop as usual, and the hanging one goes back to NULL by itself once
    /// its open returns. (Opened one after another on the monitor's thread,
    /// nothing was metered and `stop` never finished.)
    #[test]
    fn a_meter_whose_open_hangs_delays_nothing() {
        let _serial = serial();
        crate::gst_init::init().expect("GStreamer");
        let gate = Arc::new(Gate::default());
        *HANG_NEXT.lock().unwrap() = Some((FAKE_MONITOR.to_owned(), Arc::clone(&gate)));
        let (tx, rx) = crossbeam_channel::unbounded();
        let monitor = MeterMonitor::start(
            vec![FAKE_MONITOR.to_owned(), FAKE_MIC.to_owned()],
            Backend::Fake,
            tx,
        )
        .expect("starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(SideEvent::Level(level)) if level.device == FAKE_MIC => break,
                Ok(SideEvent::Level(level)) => panic!("a reading of {}", level.device),
                Ok(_) => {}
                Err(_) => panic!("no reading of {FAKE_MIC} within 5 s"),
            }
        }
        let stopping = Instant::now();
        monitor.stop();
        while !monitor.is_finished() {
            assert!(
                stopping.elapsed() < TEARDOWN_TIMEOUT + Duration::from_secs(1),
                "not stopped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!gate.nulled());
        gate.release();
        let released = Instant::now();
        while !gate.nulled() {
            assert!(
                released.elapsed() < Duration::from_secs(5),
                "the meter left behind never went back to NULL"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        rx.try_iter().for_each(drop);
        std::thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err(), "a reading after the stop");
    }

    #[test]
    fn no_devices_start_nothing() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let monitor = MeterMonitor::start(Vec::new(), Backend::Fake, tx).expect("starts");
        assert!(monitor.is_finished());
        monitor.stop();
        assert!(rx.try_recv().is_err(), "no readings");
    }
}
