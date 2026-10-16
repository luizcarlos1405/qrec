//! The media hub (G§3.4.2, G§3.5, G§3.9): everything that moves pictures
//! and sound for the window, behind one cheap handle in `Ctx`.
//!
//! - **Pictures.** [`MediaHub::live_paintable`] and
//!   [`MediaHub::program_paintable`] are stable `gdk::Paintable`s for the
//!   stage. Before the engine composites into the GUI's sink (WP1), the live
//!   one shows the engine's RGBA appsink frames (`SideEvent::Preview`) as
//!   `gdk::MemoryTexture`s; afterwards it shows the live
//!   `gtk4paintablesink`'s paintable. The program one shows the program
//!   sink's once the player draws into it.
//! - **Sinks.** Both `gtk4paintablesink`s are made on the GTK thread after
//!   GStreamer initialized (startup step 5: a sink made on a worker
//!   deadlocks) and kept until exit (R4).
//! - **Side pump.** Thread `qrec-side` drains the engine's and the meter
//!   monitor's side channel into latest-wins slots (preview frame, levels
//!   plus a 15 min ring per device, live statuses) and wakes the main thread
//!   once per batch; live errors become a warning toast. High-rate data
//!   never enters the queue.
//! - **Meters** (D-03): the meter monitor runs exactly when the Session is
//!   open, nothing records and the canvas shows Program (the capture
//!   pipeline is down then), on the project scene's audio sources.
//! - **Player and caches** (step 8): the program player and the preview
//!   cache start once the Session is open; [`MediaHub::shutdown`] stops them
//!   at exit, asynchronously.
//! - **`qrec-gui-io`**: one worker thread for blocking jobs of the window
//!   (bin sizes, silence keys, wl-copy, source discovery):
//!   [`MediaHub::run_io`] and [`MediaHub::run_io_then`]. The preview
//!   caches' disk trim runs on the cache's own `qrec-preview-trim`.

pub mod live;
pub mod peaks;
pub mod program;
pub mod thumbs;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use crossbeam_channel::Sender;
use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::protocol::{Toast, ToastLevel};
use qrec_core::state::{CanvasMode, JobKind, State};
use qrec_shell::capture::meters::MeterMonitor;
use qrec_shell::capture::{
    Backend, LevelReading, LiveControl, LiveSourceStatus, PreviewFrame, SideEvent,
};
use qrec_shell::media::preview::{PreviewCache, PreviewData};
use qrec_shell::player::{AudioOut, PlayerOptions, ProgramPlayer};

use crate::guard;
use crate::host::queue::{Inbound, Queue};
use crate::store::{Store, Subscribers, Subscription};
use peaks::PeakCache;
use thumbs::ThumbCache;

/// How many level readings a device's ring keeps: 15 min at 20 Hz (the
/// ghost waveform of a long take).
pub const LEVEL_HISTORY: usize = 15 * 60 * 20;

/// A job for `qrec-gui-io`.
pub type IoJob = Box<dyn FnOnce() + Send>;

/// One level reading, for the ring: the loudest channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelSample {
    pub rms_db: f32,
    pub peak_db: f32,
}

/// What the side pump keeps for the main thread (latest wins).
#[derive(Debug, Default)]
pub struct SideSlots {
    /// The newest live preview frame not shown yet.
    pub preview: Option<PreviewFrame>,
    /// The newest reading per device.
    pub levels: BTreeMap<String, LevelReading>,
    /// The last [`LEVEL_HISTORY`] readings per device.
    pub history: BTreeMap<String, VecDeque<LevelSample>>,
    /// The newest live statuses not taken yet.
    pub live_status: Option<Vec<LiveSourceStatus>>,
}

impl SideSlots {
    /// Takes one side event in (the pump's work, testable without GTK).
    /// Returns the live error to toast, if it is one.
    pub fn absorb(&mut self, event: SideEvent) -> Option<String> {
        match event {
            SideEvent::Preview(frame) => self.preview = Some(frame),
            SideEvent::Level(reading) => {
                let loudest = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let sample = LevelSample {
                    rms_db: loudest(&reading.rms_db) as f32,
                    peak_db: loudest(&reading.peak_db) as f32,
                };
                let ring = self.history.entry(reading.device.clone()).or_default();
                if ring.len() == LEVEL_HISTORY {
                    ring.pop_front();
                }
                ring.push_back(sample);
                self.levels.insert(reading.device.clone(), reading);
            }
            SideEvent::LiveStatus(statuses) => self.live_status = Some(statuses),
            SideEvent::LiveError(message) => return Some(message),
        }
        None
    }
}

thread_local! {
    /// The hub of this (the main) thread, for the side pump's wake-up.
    static HUB: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

/// The window's media. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct MediaHub {
    inner: Rc<Inner>,
}

struct Inner {
    queue: Queue,
    store: Store,
    backend: Backend,
    live: FramePaintable,
    program: FramePaintable,
    live_sink: RefCell<Option<gst::Element>>,
    program_sink: RefCell<Option<gst::Element>>,
    side: RefCell<Option<Sender<SideEvent>>>,
    slots: Arc<Mutex<SideSlots>>,
    side_pending: Arc<AtomicBool>,
    live_control: RefCell<LiveControl>,
    meters: RefCell<Option<(Vec<String>, MeterMonitor)>>,
    player: RefCell<Option<Rc<ProgramPlayer>>>,
    preview: RefCell<Option<PreviewCache>>,
    thumbs: RefCell<Option<Rc<ThumbCache>>>,
    peaks: RefCell<Option<Rc<PeakCache>>>,
    preview_data: Subscribers<PreviewData>,
    io: RefCell<Option<Sender<IoJob>>>,
    frames_shown: Cell<u64>,
    subscription: RefCell<Option<Subscription>>,
}

impl std::fmt::Debug for MediaHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaHub")
            .field("sinks", &self.inner.live_sink.borrow().is_some())
            .field("player", &self.inner.player.borrow().is_some())
            .field("meters", &self.inner.meters.borrow().as_ref().map(|m| &m.0))
            .finish_non_exhaustive()
    }
}

impl MediaHub {
    /// The hub of this window; follows `store` to run the meter monitor.
    pub fn new(queue: Queue, store: Store) -> Self {
        let hub = Self {
            inner: Rc::new(Inner {
                queue,
                store: store.clone(),
                backend: Backend::from_env(),
                live: FramePaintable::new(),
                program: FramePaintable::new(),
                live_sink: RefCell::new(None),
                program_sink: RefCell::new(None),
                side: RefCell::new(None),
                slots: Arc::new(Mutex::new(SideSlots::default())),
                side_pending: Arc::new(AtomicBool::new(false)),
                live_control: RefCell::new(LiveControl::noop()),
                meters: RefCell::new(None),
                player: RefCell::new(None),
                preview: RefCell::new(None),
                thumbs: RefCell::new(None),
                peaks: RefCell::new(None),
                preview_data: Subscribers::default(),
                io: RefCell::new(None),
                frames_shown: Cell::new(0),
                subscription: RefCell::new(None),
            }),
        };
        HUB.with(|h| *h.borrow_mut() = Rc::downgrade(&hub.inner));
        let weak = Rc::downgrade(&hub.inner);
        let sub = store.subscribe(move |change| {
            if let Some(inner) = weak.upgrade() {
                MediaHub { inner }.follow(&change.next);
            }
        });
        *hub.inner.subscription.borrow_mut() = Some(sub);
        hub
    }

    /// The live picture: the live sink's paintable, which the engine
    /// composites into (WP1; the appsink's frames while
    /// `capture::PREVIEW_SINK_SUPPORTED` is false). Stable.
    pub fn live_paintable(&self) -> gdk::Paintable {
        self.inner.live.clone().upcast()
    }

    /// The Program picture (the program sink's paintable). Stable.
    pub fn program_paintable(&self) -> gdk::Paintable {
        self.inner.program.clone().upcast()
    }

    /// The newest level of `device` (meters read it on each frame tick).
    pub fn level(&self, device: &str) -> Option<LevelReading> {
        self.lock_slots().levels.get(device).cloned()
    }

    /// The last `last` readings of `device`, oldest first.
    pub fn level_history(&self, device: &str, last: usize) -> Vec<LevelSample> {
        let slots = self.lock_slots();
        slots.history.get(device).map_or_else(Vec::new, |ring| {
            ring.iter()
                .skip(ring.len().saturating_sub(last))
                .copied()
                .collect()
        })
    }

    /// Moves the live preview's layers during a Live drag (D-32, D-33).
    pub fn live_control(&self) -> LiveControl {
        self.inner.live_control.borrow().clone()
    }

    /// The program player, once the Session is open and if it started.
    pub fn player(&self) -> Option<Rc<ProgramPlayer>> {
        self.inner.player.borrow().clone()
    }

    /// The player's position, if there is a player.
    pub fn position_ns(&self) -> Option<u64> {
        self.player().map(|p| p.position_ns())
    }

    /// The preview cache, once the Session is open.
    pub fn preview_cache(&self) -> Option<PreviewCache> {
        self.inner.preview.borrow().clone()
    }

    /// The thumbnail cache (WP2), once the Session is open.
    pub fn thumbs(&self) -> Option<Rc<ThumbCache>> {
        self.inner.thumbs.borrow().clone()
    }

    /// The peak cache (WP2), once the Session is open.
    pub fn peaks(&self) -> Option<Rc<PeakCache>> {
        self.inner.peaks.borrow().clone()
    }

    /// Follows every answer of the preview cache (silence keys, failures);
    /// the texture and peak caches get theirs first.
    pub fn subscribe_preview(&self, f: impl Fn(&PreviewData) + 'static) -> Subscription {
        self.inner.preview_data.add(f)
    }

    /// An answer of the preview cache, from the queue.
    pub fn on_preview(&self, data: &PreviewData) {
        if let Some(thumbs) = self.thumbs() {
            thumbs.on_data(data);
        }
        if let Some(peaks) = self.peaks() {
            peaks.on_data(data);
        }
        self.inner.preview_data.emit(data);
    }

    /// The live `gtk4paintablesink` (startup step 5), if it was made.
    pub fn live_sink(&self) -> Option<gst::Element> {
        self.inner.live_sink.borrow().clone()
    }

    /// The program `gtk4paintablesink`, if it was made.
    pub fn program_sink(&self) -> Option<gst::Element> {
        self.inner.program_sink.borrow().clone()
    }

    /// Live preview frames shown so far (the appsink path).
    pub fn frames_shown(&self) -> u64 {
        self.inner.frames_shown.get()
    }

    /// Startup step 5, on the GTK thread once GStreamer initialized: makes
    /// both `gtk4paintablesink`s and reads their paintables (main thread
    /// only). The program picture shows the program sink's paintable; the
    /// live one switches to the live sink's once the engine composites
    /// into it.
    pub fn create_sinks(&self) -> Result<(), String> {
        let make = |name: &str| -> Result<(gst::Element, gdk::Paintable), String> {
            let sink = gst::ElementFactory::make("gtk4paintablesink")
                .name(name)
                .build()
                .map_err(|e| format!("gtk4paintablesink: {e}"))?;
            let paintable = sink.property::<gdk::Paintable>("paintable");
            Ok((sink, paintable))
        };
        let (live, live_paintable) = make("qrec-live-sink")?;
        let (program, program_paintable) = make("qrec-program-sink")?;
        if qrec_shell::capture::PREVIEW_SINK_SUPPORTED {
            self.inner.live.set_source(Some(&live_paintable));
        }
        self.inner.program.set_source(Some(&program_paintable));
        *self.inner.live_sink.borrow_mut() = Some(live);
        *self.inner.program_sink.borrow_mut() = Some(program);
        Ok(())
    }

    /// The side channel the engine and the meter monitor send to; starts
    /// the `qrec-side` pump on first use.
    pub fn side_sender(&self) -> Sender<SideEvent> {
        if let Some(tx) = &*self.inner.side.borrow() {
            return tx.clone();
        }
        let (tx, rx) = crossbeam_channel::unbounded::<SideEvent>();
        let slots = Arc::clone(&self.inner.slots);
        let pending = Arc::clone(&self.inner.side_pending);
        let queue = self.inner.queue.clone();
        let spawned = thread::Builder::new()
            .name("qrec-side".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut errors = Vec::new();
                    {
                        let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
                        errors.extend(slots.absorb(first));
                        while let Ok(next) = rx.try_recv() {
                            errors.extend(slots.absorb(next));
                        }
                    }
                    for message in errors {
                        queue.push(Inbound::Toast {
                            toast: Toast::new(ToastLevel::Warning, "Live preview problem")
                                .with_body(message),
                            recovery: false,
                        });
                    }
                    if !pending.swap(true, Ordering::SeqCst) {
                        guard::idle(glib::Priority::DEFAULT, "the side pump's wake", || {
                            side_wake();
                            glib::ControlFlow::Break
                        });
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("can't start the side pump: {e}");
        }
        *self.inner.side.borrow_mut() = Some(tx.clone());
        tx
    }

    /// The engine's live control (from the opener, through the queue).
    pub fn set_live_control(&self, control: LiveControl) {
        *self.inner.live_control.borrow_mut() = control;
    }

    /// Step 8, once the Session is open: the program player and the
    /// preview cache.
    pub fn on_opened(&self, project_dir: &Path) {
        if self.inner.player.borrow().is_none()
            && let Some(sink) = self.program_sink()
        {
            let queue = self.inner.queue.clone();
            let opts = PlayerOptions {
                project_dir: project_dir.to_path_buf(),
                video_sink: sink,
                audio: match self.inner.backend {
                    Backend::Fake => AudioOut::Silent,
                    Backend::Real => AudioOut::System,
                },
                out_size: None,
                events: Box::new(move |event| queue.push(Inbound::Player(event))),
            };
            match ProgramPlayer::spawn(opts) {
                Ok(player) => *self.inner.player.borrow_mut() = Some(Rc::new(player)),
                Err(e) => tracing::info!("no Program player: {e}"),
            }
        }
        if self.inner.preview.borrow().is_none() {
            let queue = self.inner.queue.clone();
            let cache = PreviewCache::new(
                PathBuf::from(project_dir),
                Box::new(move |data| queue.push(Inbound::Preview(data))),
            );
            *self.inner.thumbs.borrow_mut() = Some(Rc::new(ThumbCache::new(
                cache.clone(),
                thumbs::BUDGET_BYTES,
            )));
            *self.inner.peaks.borrow_mut() =
                Some(Rc::new(PeakCache::new(cache.clone(), peaks::BUDGET_BYTES)));
            *self.inner.preview.borrow_mut() = Some(cache);
        }
        self.follow(&self.inner.store.state());
    }

    /// Nothing the preview cache was asked for is queued, running or still
    /// being handed to the queue (the test channel's `waitIdle`, G§3.18):
    /// no cache yet, or `PreviewCache::in_flight() == 0`. An answer is in
    /// the queue before its job stops counting, so a check of this before
    /// the queue's misses none.
    pub fn previews_idle(&self) -> bool {
        self.inner
            .preview
            .borrow()
            .as_ref()
            .is_none_or(|cache| cache.in_flight() == 0)
    }

    /// At exit (R4): the meters, the player and the cache stop; each
    /// reaches NULL on its own thread (≤ 5 s) while the main loop runs.
    pub fn shutdown(&self) {
        if let Some((_, monitor)) = self.inner.meters.borrow_mut().take() {
            monitor.stop();
        }
        if let Some(player) = self.inner.player.borrow().as_ref() {
            player.shutdown();
        }
        if let Some(cache) = self.inner.preview.borrow().as_ref() {
            cache.shutdown();
        }
    }

    /// Runs `job` on `qrec-gui-io`, after the jobs before it.
    pub fn run_io(&self, job: impl FnOnce() + Send + 'static) {
        let tx = self.io_sender();
        if let Err(e) = tx.send(Box::new(job)) {
            tracing::error!("the io worker is gone: {e}");
        }
    }

    /// Runs `work` on `qrec-gui-io`, then `then` with its result on the
    /// main thread.
    pub fn run_io_then<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(T) + 'static,
    ) {
        let then = glib::thread_guard::ThreadGuard::new(then);
        self.run_io(move || {
            let result = work();
            guard::invoke("an io job's result", move || (then.into_inner())(result));
        });
    }

    fn io_sender(&self) -> Sender<IoJob> {
        if let Some(tx) = &*self.inner.io.borrow() {
            return tx.clone();
        }
        let (tx, rx) = crossbeam_channel::unbounded::<IoJob>();
        let spawned = thread::Builder::new()
            .name("qrec-gui-io".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            });
        if let Err(e) = spawned {
            tracing::error!("can't start the io worker: {e}");
        }
        *self.inner.io.borrow_mut() = Some(tx.clone());
        tx
    }

    fn lock_slots(&self) -> std::sync::MutexGuard<'_, SideSlots> {
        self.inner
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The side pump's batch, on the main thread.
    fn on_side_wake(&self) {
        self.inner.side_pending.store(false, Ordering::SeqCst);
        let (frame, statuses) = {
            let mut slots = self.lock_slots();
            (slots.preview.take(), slots.live_status.take())
        };
        if let Some(frame) = frame {
            self.show_frame(frame);
        }
        if let Some(statuses) = statuses {
            self.inner
                .store
                .update_local(move |l| l.live_status = Arc::new(statuses));
        }
    }

    /// An appsink frame as the live picture (until the engine composites
    /// into the live sink).
    fn show_frame(&self, frame: PreviewFrame) {
        if self.inner.live.has_source() {
            return;
        }
        let (Ok(width), Ok(height)) = (i32::try_from(frame.width), i32::try_from(frame.height))
        else {
            return;
        };
        let stride = frame.stride as usize;
        if width == 0 || height == 0 || frame.rgba.len() < stride * frame.height as usize {
            return;
        }
        let bytes = glib::Bytes::from_owned(frame.rgba);
        let texture =
            gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8, &bytes, stride);
        self.inner.live.set_texture(Some(texture.upcast()));
        self.inner
            .frames_shown
            .set(self.inner.frames_shown.get() + 1);
    }

    /// Runs the meter monitor exactly when it is wanted (D-03), and keeps
    /// the preview cache to one ffmpeg while an export runs (G§3.11).
    fn follow(&self, state: &State) {
        if let Some(cache) = self.preview_cache() {
            let exporting = state
                .runtime
                .jobs
                .values()
                .any(|job| matches!(job.kind, JobKind::Export { .. }));
            cache.set_exporting(exporting);
        }
        let open = self.inner.store.local().is_open();
        let wanted =
            (open && state.runtime.record.is_idle() && state.ui.canvas_mode == CanvasMode::Program)
                .then(|| meter_devices(state));
        let current = self.inner.meters.borrow().as_ref().map(|(d, _)| d.clone());
        if wanted == current {
            return;
        }
        if let Some((_, monitor)) = self.inner.meters.borrow_mut().take() {
            monitor.stop();
        }
        let Some(devices) = wanted.filter(|d| !d.is_empty()) else {
            return;
        };
        match MeterMonitor::start(devices.clone(), self.inner.backend, self.side_sender()) {
            Ok(monitor) => *self.inner.meters.borrow_mut() = Some((devices, monitor)),
            Err(e) => tracing::warn!("meters: can't start the monitor: {e}"),
        }
    }
}

/// The audio sources the meter monitor follows: the project scene's
/// tracks, in order, once each.
pub fn meter_devices(state: &State) -> Vec<String> {
    let mut devices: Vec<String> = Vec::new();
    for track in &state.project.scene.audio {
        if !devices.contains(&track.source) {
            devices.push(track.source.clone());
        }
    }
    devices
}

/// The side pump's wake-up, on the main thread.
fn side_wake() {
    let hub = HUB.with(|h| h.borrow().upgrade());
    if let Some(inner) = hub {
        MediaHub { inner }.on_side_wake();
    }
}

// ------------------------------------------------------------ the picture

mod imp {
    use std::cell::RefCell;

    use gtk::gdk;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    /// See [`super::FramePaintable`].
    #[derive(Default)]
    pub struct FramePaintable {
        pub texture: RefCell<Option<gdk::Paintable>>,
        pub source: RefCell<Option<(gdk::Paintable, Vec<glib::SignalHandlerId>)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FramePaintable {
        const NAME: &'static str = "QrecFramePaintable";
        type Type = super::FramePaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for FramePaintable {
        fn dispose(&self) {
            crate::guard::run("picture: dispose", || {
                let taken = self.source.borrow_mut().take();
                if let Some((source, handlers)) = taken {
                    for handler in handlers {
                        source.disconnect(handler);
                    }
                }
            });
        }
    }

    impl FramePaintable {
        fn current(&self) -> Option<gdk::Paintable> {
            match &*self.source.borrow() {
                Some((source, _)) => Some(source.clone()),
                None => self.texture.borrow().clone(),
            }
        }
    }

    impl PaintableImpl for FramePaintable {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            crate::guard::run("picture: snapshot", || {
                if let Some(current) = self.current() {
                    current.snapshot(snapshot, width, height);
                }
            });
        }

        fn intrinsic_width(&self) -> i32 {
            crate::guard::run("picture: width", || {
                self.current().map_or(0, |p| p.intrinsic_width())
            })
        }

        fn intrinsic_height(&self) -> i32 {
            crate::guard::run("picture: height", || {
                self.current().map_or(0, |p| p.intrinsic_height())
            })
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            crate::guard::run("picture: aspect", || {
                self.current().map_or(0.0, |p| p.intrinsic_aspect_ratio())
            })
        }

        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::empty()
        }
    }
}

glib::wrapper! {
    /// A stable picture for the stage: the newest texture handed to it, or
    /// another paintable it follows (a `gtk4paintablesink`'s).
    pub struct FramePaintable(ObjectSubclass<imp::FramePaintable>)
        @implements gdk::Paintable;
}

impl Default for FramePaintable {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePaintable {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `texture` (unless it follows a source).
    pub fn set_texture(&self, texture: Option<gdk::Paintable>) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let size_changed = {
            let old = self.imp().texture.borrow();
            let dims = |p: &Option<gdk::Paintable>| {
                p.as_ref()
                    .map(|p| (p.intrinsic_width(), p.intrinsic_height()))
            };
            dims(&old) != dims(&texture)
        };
        *self.imp().texture.borrow_mut() = texture;
        if size_changed {
            self.invalidate_size();
        }
        self.invalidate_contents();
    }

    /// Follows `source` instead of the textures (`None`: back to them).
    pub fn set_source(&self, source: Option<&gdk::Paintable>) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        if let Some((old, handlers)) = self.imp().source.borrow_mut().take() {
            for handler in handlers {
                old.disconnect(handler);
            }
        }
        if let Some(source) = source {
            let weak = self.downgrade();
            let contents = source.connect_invalidate_contents(crate::guarded!(
                "picture: contents",
                move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.invalidate_contents();
                    }
                }
            ));
            let weak = self.downgrade();
            let size =
                source.connect_invalidate_size(crate::guarded!("picture: size", move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.invalidate_size();
                    }
                }));
            *self.imp().source.borrow_mut() = Some((source.clone(), vec![contents, size]));
        }
        self.invalidate_size();
        self.invalidate_contents();
    }

    /// It follows another paintable.
    pub fn has_source(&self) -> bool {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().source.borrow().is_some()
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::testkit;

    use super::*;

    fn level(device: &str, rms: f64) -> SideEvent {
        SideEvent::Level(LevelReading {
            device: device.into(),
            rms_db: vec![rms, rms - 3.0],
            peak_db: vec![rms + 6.0],
        })
    }

    #[test]
    fn side_slots_keep_the_newest_and_a_bounded_ring() {
        let mut slots = SideSlots::default();
        assert_eq!(slots.absorb(level("mic", -30.0)), None);
        assert_eq!(slots.absorb(level("mic", -20.0)), None);
        assert_eq!(slots.levels["mic"].rms_db[0], -20.0);
        assert_eq!(
            slots.history["mic"]
                .iter()
                .map(|s| s.rms_db)
                .collect::<Vec<_>>(),
            [-30.0, -20.0]
        );
        for _ in 0..LEVEL_HISTORY {
            slots.absorb(level("mic", -10.0));
        }
        assert_eq!(slots.history["mic"].len(), LEVEL_HISTORY);
        assert_eq!(
            slots.absorb(SideEvent::LiveError("the camera died".into())),
            Some("the camera died".into())
        );
        slots.absorb(SideEvent::LiveStatus(Vec::new()));
        assert_eq!(slots.live_status, Some(Vec::new()));
        let frame = |n: u64| PreviewFrame {
            width: 2,
            height: 1,
            stride: 8,
            rgba: vec![0; 8],
            pts_ns: Some(n),
        };
        slots.absorb(SideEvent::Preview(frame(1)));
        slots.absorb(SideEvent::Preview(frame(2)));
        assert_eq!(slots.preview.unwrap().pts_ns, Some(2), "latest wins");
    }

    #[test]
    fn meters_follow_the_project_scenes_sources() {
        let state = testkit::design_state();
        let devices = meter_devices(&state);
        let expected: Vec<String> = state
            .project
            .scene
            .audio
            .iter()
            .map(|t| t.source.clone())
            .collect();
        assert_eq!(devices, expected);
        assert!(!devices.is_empty());
    }
}
