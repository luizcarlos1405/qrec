//! The Program player against real GES pipelines (docs/GUI.md G§3.10,
//! WP1): an `appsink` stands in for the paintable sink and reads the
//! frame-ID fixture (the source frame index drawn as binary bars), so every
//! check is about the exact frame shown. GStreamer present; silent sound.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qrec_core::TakeId;
use qrec_core::ges_plan::plan_ges;
use qrec_core::layout::{Placement, SourceRegion};
use qrec_core::model::Rect;
use qrec_core::plans::GesVideoClip;

use super::*;
use crate::ges_render::parity_tests::{
    CANVAS, FPS, canvas, frame_id_fixture, parity_project, placements, read_frame_id, take_frame,
};
use crate::gst_init::testing::{assert_quiet_since, problems, serial};
use crate::testkit::TestEnv;

/// A load or seek is done well within this on any machine.
const SETTLE: Duration = Duration::from_secs(20);

fn ns(frame: u64) -> u64 {
    frame_ns(frame as i64, FPS) as u64
}

/// Polls `done` every 5 ms; panics after `timeout`.
fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "{what}: not within {timeout:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// A frame the test sink got.
#[derive(Debug, Clone, Copy)]
struct Seen {
    pts: u64,
    width: u32,
    height: u32,
    /// The frame ID read at the tap's rect.
    id: Option<u64>,
}

/// `videoconvert ! video/x-raw,format=RGB ! appsink` recording every frame
/// (preroll included) and the frame ID drawn at `rect`.
struct Tap {
    sink: gst::Element,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Tap {
    fn new(rect: Option<Rect>) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = {
            let seen = Arc::clone(&seen);
            Arc::new(move |sample: gst::Sample| {
                let caps = sample.caps().expect("caps");
                let info = gst_video::VideoInfo::from_caps(caps).expect("video caps");
                let buffer = sample.buffer().expect("a buffer");
                let map = buffer.map_readable().expect("readable");
                let stride = info.stride()[0] as usize;
                let data = map.as_slice();
                let id = rect
                    .map(|r| read_frame_id(|x, y| data[y as usize * stride + x as usize * 3], r));
                seen.lock().unwrap().push(Seen {
                    pts: buffer.pts().map_or(u64::MAX, gst::ClockTime::nseconds),
                    width: info.width(),
                    height: info.height(),
                    id,
                });
            })
        };
        let on_preroll = Arc::clone(&record);
        let appsink = gst_app::AppSink::builder()
            .sync(true)
            .max_buffers(2)
            .drop(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_preroll(move |sink| {
                        if let Ok(sample) = sink.pull_preroll() {
                            on_preroll(sample);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        record(sample);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build();
        let convert = gst::ElementFactory::make("videoconvert").build().unwrap();
        let filter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "RGB")
                    .build(),
            )
            .build()
            .unwrap();
        let bin = gst::Bin::with_name("qrec-test-tap");
        bin.add_many([&convert, &filter, appsink.upcast_ref()])
            .unwrap();
        gst::Element::link_many([&convert, &filter, appsink.upcast_ref()]).unwrap();
        let ghost = gst::GhostPad::with_target(&convert.static_pad("sink").unwrap()).unwrap();
        bin.add_pad(&ghost).unwrap();
        Self {
            sink: bin.upcast(),
            seen,
        }
    }

    fn all(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn last(&self) -> Option<Seen> {
        self.seen.lock().unwrap().last().copied()
    }

    /// Waits until the newest frame is at `pts`.
    fn showing(&self, pts: u64) -> Seen {
        wait_until(&format!("a frame at {pts} ns"), SETTLE, || {
            self.last().is_some_and(|s| s.pts == pts)
        });
        self.last().unwrap()
    }
}

/// The player's events, as they come.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<PlayerEvent>>>);

impl Events {
    fn callback(&self) -> Box<dyn Fn(PlayerEvent) + Send + Sync> {
        let events = self.clone();
        Box::new(move |event| events.0.lock().unwrap().push(event))
    }

    fn all(&self) -> Vec<PlayerEvent> {
        self.0.lock().unwrap().clone()
    }

    fn count(&self, matches: impl Fn(&PlayerEvent) -> bool) -> usize {
        self.all().iter().filter(|e| matches(e)).count()
    }

    /// Waits for the `n`th event that `matches` (1-based) and returns it.
    fn nth(&self, n: usize, what: &str, matches: impl Fn(&PlayerEvent) -> bool) -> PlayerEvent {
        wait_until(what, SETTLE, || self.count(&matches) >= n);
        self.all()
            .into_iter()
            .filter(|e| matches(e))
            .nth(n - 1)
            .unwrap()
    }

    fn loaded(&self, n: usize) -> u64 {
        match self.nth(n, "Loaded", |e| matches!(e, PlayerEvent::Loaded { .. })) {
            PlayerEvent::Loaded { duration_ns } => duration_ns,
            _ => unreachable!(),
        }
    }

    fn errors(&self) -> Vec<String> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                PlayerEvent::Error { message } => Some(message),
                _ => None,
            })
            .collect()
    }
}

fn spawn(tap: &Tap, dir: &Path, out_size: Option<(u32, u32)>, events: &Events) -> ProgramPlayer {
    ProgramPlayer::spawn(PlayerOptions {
        project_dir: dir.to_owned(),
        video_sink: tap.sink.clone(),
        audio: AudioOut::Silent,
        out_size,
        events: events.callback(),
    })
    .expect("the player thread starts")
}

fn settle(player: &ProgramPlayer) {
    wait_until("the player settles", SETTLE, || player.idle());
}

/// Shuts `player` down and checks it stopped within 5 s.
fn stop(player: ProgramPlayer, events: &Events) {
    let asked = Instant::now();
    player.shutdown();
    events.nth(1, "Stopped", |e| *e == PlayerEvent::Stopped);
    assert!(
        asked.elapsed() <= STOP_TIMEOUT,
        "stopped after {:?}",
        asked.elapsed()
    );
}

/// A plan of `frames` frames of a 160×120 frame-ID video filling a
/// 160×120 canvas from its first frame: timeline frame `f` shows source
/// frame `f`.
fn frame_id_plan(dir: &Path, frames: u64) -> GesTimeline {
    fs::copy(frame_id_fixture(160, 120, FPS, frames), dir.join("ids.mkv")).unwrap();
    let rect = Rect::new(0, 0, 160, 120);
    GesTimeline {
        canvas: canvas(160, 120, FPS),
        duration_ns: ns(frames),
        video: vec![GesVideoClip {
            take: TakeId::numbered(1),
            layer: "L1".parse().expect("a layer id"),
            file: "ids.mkv".into(),
            layer_priority: 0,
            start_ns: 0,
            inpoint_ns: 0,
            duration_ns: ns(frames),
            placement: Placement {
                src: SourceRegion {
                    x: 0,
                    y: 0,
                    w: 160,
                    h: 120,
                },
                dst: rect,
            },
            alpha: 1.0,
        }],
        audio: Vec::new(),
    }
}

const ID_RECT: Rect = Rect::new(0, 0, 160, 120);

/// A§11 parity, played: after each accurate seek the picture is the frame
/// plan's frame, in every rounding phase of 30 fps Matroska, across cuts
/// and at the take's last frame; a seek past the end shows the last frame.
#[test]
fn accurate_seeks_show_the_frame_plans_frames() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let p = parity_project();
    let camera = placements(&p.project)
        .into_iter()
        .find(|l| l.0 == "L2")
        .unwrap()
        .2
        .dst;
    let tap = Tap::new(Some(camera));
    let events = Events::default();
    let player = spawn(&tap, &p.dir, None, &events);
    let plan = plan_ges(&p.project);
    player.load(plan.clone(), 0);
    assert_eq!(events.loaded(1), plan.duration_ns);
    for at in [0u64, 1, 2, 3, 29, 30, 31, 32, 54, 55, 63, 64, 10, 0] {
        player.seek(ns(at));
        settle(&player);
        assert_eq!(player.position_ns(), ns(at));
        let seen = tap.showing(ns(at));
        assert_eq!((seen.width, seen.height), CANVAS);
        assert_eq!(
            seen.id,
            Some(take_frame(&p.project, at)),
            "timeline frame {at}"
        );
    }
    // Past the end: the last frame shows, the position is the end.
    player.seek(plan.duration_ns + 1_000_000);
    settle(&player);
    assert_eq!(player.position_ns(), plan.duration_ns);
    assert_eq!(tap.showing(ns(64)).id, Some(take_frame(&p.project, 64)));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// A load shows the new plan at the position asked and keeps playing or
/// paused as before; frames played after it are the new plan's.
#[test]
fn a_load_keeps_the_position_and_playing() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let p = parity_project();
    let camera = placements(&p.project)
        .into_iter()
        .find(|l| l.0 == "L2")
        .unwrap()
        .2
        .dst;
    // The same take without its first clip: frame f shows take frame 55 + f.
    let mut cut = p.project.clone();
    cut.timeline.clips.retain(|c| c.in_ >= 55);
    let tap = Tap::new(Some(camera));
    let events = Events::default();
    let player = spawn(&tap, &p.dir, None, &events);
    player.load(plan_ges(&p.project), ns(20));
    events.loaded(1);
    assert_eq!(tap.showing(ns(20)).id, Some(take_frame(&p.project, 20)));
    assert_eq!(player.position_ns(), ns(20));

    // Paused: the new plan at the same position.
    player.load(plan_ges(&cut), ns(20));
    assert_eq!(events.loaded(2), ns(35));
    assert_eq!(tap.showing(ns(20)).id, Some(75));
    settle(&player);
    assert_eq!(player.position_ns(), ns(20));

    // Playing: a load goes on playing from where it was asked.
    player.load(plan_ges(&p.project), 0);
    events.loaded(3);
    player.play(1.0);
    wait_until("playing past frame 10", SETTLE, || {
        player.position_ns() > ns(10)
    });
    let at = player.position_ns();
    let before = tap.all().len();
    player.load(plan_ges(&p.project), at);
    events.loaded(4);
    wait_until("playing on after the load", SETTLE, || {
        player.position_ns() > at + ns(10)
    });
    player.pause();
    settle(&player);
    let after: Vec<Seen> = tap.all()[before..].to_vec();
    assert!(after.len() > 5, "{after:?}");
    for seen in &after {
        let frame = seen.pts / ns(1).max(1);
        if seen.pts == ns(frame) {
            assert_eq!(seen.id, Some(take_frame(&p.project, frame)), "{seen:?}");
        }
    }
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// The playback rate measured over `window` of playing: timeline time per
/// wall time.
fn measured_rate(player: &ProgramPlayer, window: Duration) -> f64 {
    let (t0, p0) = (Instant::now(), player.position_ns());
    thread::sleep(window);
    let (t1, p1) = (Instant::now(), player.position_ns());
    (p1 as f64 - p0 as f64) / (t1 - t0).as_nanos() as f64
}

/// Rates 1, 2 and 4, changed while playing: the position runs that fast and
/// every frame shown is the one its time says; a pause holds it.
#[test]
fn rates_change_while_playing() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("rates");
    let plan = frame_id_plan(&dir, 600);
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan, 0);
    events.loaded(1);
    for rate in [1.0, 2.0, 4.0] {
        player.play(rate);
        thread::sleep(Duration::from_millis(300));
        let measured = measured_rate(&player, Duration::from_millis(700));
        assert!(
            (measured - rate).abs() <= rate * 0.25,
            "playing at {rate}: the position ran at {measured}"
        );
    }
    player.pause();
    settle(&player);
    thread::sleep(Duration::from_millis(100));
    let held = player.position_ns();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(player.position_ns(), held, "paused");
    let frames = tap.all();
    assert!(frames.len() > 30, "{} frames", frames.len());
    for seen in &frames {
        let frame = seen.pts / ns(1);
        if seen.pts == ns(frame) {
            assert_eq!(seen.id, Some(frame), "{seen:?}");
        }
    }
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// Reverse stepping: frame-accurate pictures going back at |rate| times
/// real time, at most 15 seeks a second, then `Ended { reverse: true }` at
/// the start.
#[test]
fn reverse_stepping_ends_at_the_start() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("reverse");
    let plan = frame_id_plan(&dir, 150);
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan, ns(90));
    events.loaded(1);
    tap.showing(ns(90));
    let before = tap.all().len();
    let started = Instant::now();
    player.play(-2.0);
    let ended = events.nth(1, "Ended", |e| matches!(e, PlayerEvent::Ended { .. }));
    let took = started.elapsed();
    assert_eq!(ended, PlayerEvent::Ended { reverse: true });
    // 3 s of timeline at twice real time.
    assert!(
        (Duration::from_millis(1200)..Duration::from_millis(2600)).contains(&took),
        "took {took:?}"
    );
    assert_eq!(player.position_ns(), 0);
    let steps: Vec<Seen> = tap.all()[before..].to_vec();
    assert!(steps.len() >= 5, "{steps:?}");
    assert!(
        steps.len() as f64 <= 15.0 * took.as_secs_f64() + 3.0,
        "{} steps in {took:?}",
        steps.len()
    );
    for pair in steps.windows(2) {
        assert!(pair[1].pts <= pair[0].pts, "going back: {pair:?}");
    }
    for seen in &steps {
        let frame = seen.pts / ns(1);
        assert_eq!(seen.pts, ns(frame), "frame-aligned: {seen:?}");
        assert_eq!(seen.id, Some(frame), "{seen:?}");
    }
    assert_eq!(steps.last().unwrap().pts, 0);
    // Playing back from the start ends at once.
    player.play(-1.0);
    events.nth(2, "Ended again", |e| matches!(e, PlayerEvent::Ended { .. }));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// Playing to the end: `Ended { reverse: false }` with the position at the
/// end; playing again there ends at once; a seek plays again.
#[test]
fn playing_to_the_end_ends() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("ended");
    let plan = frame_id_plan(&dir, 90);
    let duration = plan.duration_ns;
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan, ns(60));
    events.loaded(1);
    let started = Instant::now();
    player.play(1.0);
    let ended = events.nth(1, "Ended", |e| matches!(e, PlayerEvent::Ended { .. }));
    assert_eq!(ended, PlayerEvent::Ended { reverse: false });
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(player.position_ns(), duration);
    // The last frame stays (never the background under it).
    assert_eq!(tap.last().unwrap().id, Some(89));
    player.play(1.0);
    events.nth(2, "Ended at once", |e| {
        matches!(e, PlayerEvent::Ended { .. })
    });
    player.seek(ns(80));
    player.play(1.0);
    events.nth(3, "Ended after a seek", |e| {
        matches!(e, PlayerEvent::Ended { .. })
    });
    assert!(tap.all().iter().any(|s| s.id == Some(85)), "played from 80");
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// A cut where the first clip plays its file to the end: every frame
/// played is a whole canvas frame of the right source frame, never the
/// 2 ms of background GES puts where the file ran out.
#[test]
fn a_cut_at_the_end_of_a_file_shows_no_background() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("cut");
    let mut plan = frame_id_plan(&dir, 90);
    fs::copy(frame_id_fixture(160, 120, FPS, 30), dir.join("short.mkv")).unwrap();
    let first = GesVideoClip {
        file: "short.mkv".into(),
        duration_ns: ns(30),
        ..plan.video[0].clone()
    };
    let second = GesVideoClip {
        start_ns: ns(30),
        inpoint_ns: ns(40),
        duration_ns: ns(30),
        ..plan.video[0].clone()
    };
    plan.video = vec![first, second];
    plan.duration_ns = ns(60);
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan, 0);
    events.loaded(1);
    player.play(1.0);
    events.nth(1, "Ended", |e| matches!(e, PlayerEvent::Ended { .. }));
    let frames = tap.all();
    assert!(frames.len() >= 55, "{} frames", frames.len());
    for seen in &frames {
        let frame = seen.pts / ns(1);
        assert_eq!(seen.pts, ns(frame), "a whole frame: {seen:?}");
        let expected = if frame < 30 { frame } else { 40 + frame - 30 };
        assert_eq!(seen.id, Some(expected), "{seen:?}");
    }
    assert_eq!(tap.last().unwrap().id, Some(69));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// A plan with a missing file is an `Error` (and no `Loaded`); the player
/// stays usable and the next good load shows.
#[test]
fn a_missing_file_is_an_error() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("missing");
    let plan = frame_id_plan(&dir, 60);
    let mut missing = plan.clone();
    missing.video[0].file = "gone.mkv".into();
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(missing, 0);
    wait_until("an error", SETTLE, || !events.errors().is_empty());
    let errors = events.errors();
    assert!(errors[0].ends_with("gone.mkv is missing"), "{errors:?}");
    settle(&player);
    assert_eq!(events.count(|e| matches!(e, PlayerEvent::Loaded { .. })), 0);
    // Play does nothing useful, and says so.
    player.play(1.0);
    events.nth(1, "Ended", |e| matches!(e, PlayerEvent::Ended { .. }));
    player.load(plan, ns(30));
    events.loaded(1);
    assert_eq!(tap.showing(ns(30)).id, Some(30));
    assert_eq!(events.errors().len(), 1);
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// An empty plan stops the picture: `Loaded` with no duration, and plays
/// end at once.
#[test]
fn an_empty_plan_shows_nothing() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("empty");
    let plan = frame_id_plan(&dir, 30);
    let empty = GesTimeline {
        duration_ns: 0,
        video: Vec::new(),
        audio: Vec::new(),
        ..plan.clone()
    };
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan.clone(), ns(10));
    events.loaded(1);
    player.load(empty, ns(10));
    assert_eq!(events.loaded(2), 0);
    assert_eq!(player.position_ns(), 0);
    player.play(1.0);
    events.nth(1, "Ended", |e| matches!(e, PlayerEvent::Ended { .. }));
    // And back.
    player.load(plan, ns(12));
    events.loaded(3);
    assert_eq!(tap.showing(ns(12)).id, Some(12));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// `out_size`: frames arrive composited at the canvas fitted into it, with
/// the frame plan's frames where the scaled layers are.
#[test]
fn out_size_composites_smaller() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let p = parity_project();
    let plan = plan_ges(&p.project);
    let small = scaled(plan.clone(), CANVAS.0 / 2, CANVAS.1 / 2);
    let camera = small
        .video
        .iter()
        .find(|c| c.layer.as_str() == "L2")
        .unwrap()
        .placement
        .dst;
    let tap = Tap::new(Some(camera));
    let events = Events::default();
    // 4:3 bounds: the 16:9 canvas fits by its width.
    let player = spawn(&tap, &p.dir, Some((CANVAS.0 / 2, CANVAS.1)), &events);
    player.load(plan, ns(31));
    events.loaded(1);
    let seen = tap.showing(ns(31));
    assert_eq!((seen.width, seen.height), (CANVAS.0 / 2, CANVAS.1 / 2));
    assert_eq!(seen.id, Some(take_frame(&p.project, 31)));
    player.seek(ns(55));
    settle(&player);
    let seen = tap.showing(ns(55));
    assert_eq!((seen.width, seen.height), (CANVAS.0 / 2, CANVAS.1 / 2));
    assert_eq!(seen.id, Some(take_frame(&p.project, 55)));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// A scrub plays a moment and comes back to the scrubbed frame; scrubs that
/// pile up end on the newest.
#[test]
fn scrubs_end_on_the_scrubbed_frame() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("scrub");
    let plan = frame_id_plan(&dir, 120);
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan, 0);
    events.loaded(1);
    player.scrub(ns(40));
    settle(&player);
    assert_eq!(player.position_ns(), ns(40));
    assert_eq!(tap.showing(ns(40)).id, Some(40));
    for frame in [50, 51, 52, 53, 70] {
        player.scrub(ns(frame));
    }
    settle(&player);
    assert_eq!(player.position_ns(), ns(70));
    assert_eq!(tap.showing(ns(70)).id, Some(70));
    // Still paused.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(tap.last().unwrap().pts, ns(70));
    assert!(events.errors().is_empty(), "{:?}", events.errors());
    stop(player, &events);
    assert_quiet_since(quiet);
}

/// Shutdown while playing reaches NULL and says `Stopped` within 5 s;
/// calls afterwards do nothing.
#[test]
fn shutdown_stops_within_5_s() {
    let _serial = serial();
    let quiet = problems();
    ges_render::init().unwrap();
    let env = TestEnv::new();
    let dir = env.empty_dir("shutdown");
    let plan = frame_id_plan(&dir, 150);
    let tap = Tap::new(Some(ID_RECT));
    let events = Events::default();
    let player = spawn(&tap, &dir, None, &events);
    player.load(plan.clone(), 0);
    events.loaded(1);
    player.play(1.0);
    wait_until("playing", SETTLE, || player.position_ns() > ns(5));
    let asked = Instant::now();
    player.shutdown();
    player.seek(0);
    player.load(plan, 0);
    events.nth(1, "Stopped", |e| *e == PlayerEvent::Stopped);
    assert!(asked.elapsed() <= STOP_TIMEOUT, "{:?}", asked.elapsed());
    assert_eq!(tap.sink.current_state(), gst::State::Null);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(events.count(|e| matches!(e, PlayerEvent::Loaded { .. })), 1);
    assert_eq!(events.count(|e| *e == PlayerEvent::Stopped), 1);
    drop(player);
    assert_quiet_since(quiet);
}

#[test]
fn late_frames_are_counted_per_element_and_reported_once_a_second() {
    gst::init().unwrap();
    let qos = |name: &str, dropped: u64| {
        let element = gst::ElementFactory::make("fakesink")
            .name(name)
            .build()
            .unwrap();
        gst::message::Qos::builder(false)
            .stats(
                gst::format::Buffers::from_u64(100),
                gst::format::Buffers::from_u64(dropped),
            )
            .src(&element)
            .build()
    };
    let mut late = Late::default();
    let now = Instant::now();
    assert_eq!(late.due(), None);
    for (name, dropped) in [("sink", 2), ("sink", 5), ("decoder", 1), ("sink", 1)] {
        let msg = qos(name, dropped);
        let gst::MessageView::Qos(q) = msg.view() else {
            unreachable!()
        };
        late.count(q);
    }
    // 2 + 3 from the sink, 1 from the decoder, then a reset sink's 1.
    assert_eq!(late.report(now), Some(7));
    assert_eq!(late.report(now), None);
    let msg = qos("decoder", 3);
    let gst::MessageView::Qos(q) = msg.view() else {
        unreachable!()
    };
    late.count(q);
    assert_eq!(late.report(now + Duration::from_millis(500)), None);
    assert_eq!(late.due(), Some(now + LATE_PERIOD));
    assert_eq!(late.report(now + LATE_PERIOD), Some(2));
    assert_eq!(late.take_total(), Some(9));
    assert_eq!(late.take_total(), None);
}

#[test]
fn a_missing_sound_output_is_an_error_not_a_panic() {
    gst::init().unwrap();
    let err = system_sink("qrecnosuchsink").unwrap_err();
    assert!(err.contains("qrecnosuchsink is missing"), "{err}");
    assert!(silent_sink().is_ok());
}

// ------------------------------------------------------------ measurement

/// WP1's measurement (docs/GUI.md G§0.4, G§3.10): `playback_fixture` (two
/// 2560×1440 screens and a 1280×720 camera, H.264 at 30 fps, on a
/// 1920×1080@30 canvas) played by the player for 30 s into a `fakesink`:
/// `QREC_MEASURE=throughput` (`sync=false`: frames per second) or
/// `lateness` (`sync=true`, `qos=true`, `max-lateness` 20 ms like a video
/// sink: frames dropped as late). `QREC_MEASURE_OUT=960x540` composites at
/// that size (H-9); `QREC_MEASURE_SECS` changes the length. Prints one JSON
/// line. Heavy: run by hand.
#[cfg(feature = "testkit")]
#[test]
#[ignore]
fn measure_playback() {
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::testkit::design::playback_fixture;

    let _serial = serial();
    ges_render::init().unwrap();
    let mode = std::env::var("QREC_MEASURE").unwrap_or_else(|_| "throughput".into());
    let sync = match mode.as_str() {
        "throughput" => false,
        "lateness" => true,
        other => panic!("QREC_MEASURE={other}: throughput or lateness"),
    };
    let out_size = std::env::var("QREC_MEASURE_OUT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (w, h) = s.split_once('x').expect("WxH");
            (w.parse::<u32>().unwrap(), h.parse::<u32>().unwrap())
        });
    let secs: f64 = std::env::var("QREC_MEASURE_SECS")
        .ok()
        .map_or(30.0, |s| s.parse().unwrap());

    let env = TestEnv::new();
    let dir = env.empty_dir("playback");
    let project = playback_fixture(&dir);
    let plan = plan_ges(&project);

    let sink = gst::ElementFactory::make("fakesink")
        .name("measure")
        .property("sync", sync)
        .property("qos", sync)
        .property("max-lateness", if sync { 20_000_000i64 } else { -1 })
        .property("enable-last-sample", false)
        .build()
        .unwrap();
    // Frames reaching the sink: how many, when the first and the last came,
    // and how many came after their time (only meaningful with sync).
    let arrived = Arc::new(AtomicU64::new(0));
    let arrived_late = Arc::new(AtomicU64::new(0));
    let times = Arc::new(Mutex::new((None::<Instant>, None::<Instant>)));
    {
        let (arrived, arrived_late, times) = (
            Arc::clone(&arrived),
            Arc::clone(&arrived_late),
            Arc::clone(&times),
        );
        let element = sink.clone();
        let pad = sink.static_pad("sink").unwrap();
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(gst::PadProbeData::Buffer(buffer)) = &info.data else {
                return gst::PadProbeReturn::Ok;
            };
            arrived.fetch_add(1, Ordering::SeqCst);
            let now = Instant::now();
            let mut t = times.lock().unwrap();
            t.0.get_or_insert(now);
            t.1 = Some(now);
            if let (Some(pts), Some(segment), Some(clock_now)) = (
                buffer.pts(),
                pad.sticky_event::<gst::event::Segment>(0),
                element.current_running_time(),
            ) && let Some(segment) = segment.segment().downcast_ref::<gst::ClockTime>()
                && let Some(due) = segment.to_running_time(pts)
                && clock_now > due + gst::ClockTime::from_mseconds(20)
            {
                arrived_late.fetch_add(1, Ordering::SeqCst);
            }
            gst::PadProbeReturn::Ok
        });
    }
    let events = Events::default();
    let player = ProgramPlayer::spawn(PlayerOptions {
        project_dir: dir.clone(),
        video_sink: sink.clone(),
        audio: AudioOut::Silent,
        out_size,
        events: events.callback(),
    })
    .unwrap();
    player.load(plan, 0);
    events.loaded(1);
    settle(&player);
    arrived.store(0, Ordering::SeqCst);
    *times.lock().unwrap() = (None, None);

    let loadavg = || {
        fs::read_to_string("/proc/loadavg")
            .unwrap()
            .trim()
            .to_owned()
    };
    let cpu = || {
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let fields: Vec<&str> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        // utime and stime: fields 14 and 15 of stat(5), 12 and 13 here.
        let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
        // SAFETY: sysconf only reads a configuration value.
        ticks as f64 / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64
    };
    let load_before = loadavg();
    let (cpu_before, started) = (cpu(), Instant::now());
    player.play(1.0);
    let deadline = started + Duration::from_secs_f64(secs);
    while Instant::now() < deadline && events.count(|e| matches!(e, PlayerEvent::Ended { .. })) == 0
    {
        thread::sleep(Duration::from_millis(50));
    }
    let wall = started.elapsed().as_secs_f64();
    let cpu_secs = cpu() - cpu_before;
    let frames = arrived.load(Ordering::SeqCst);
    let (first, last) = *times.lock().unwrap();
    // Read while playing: a state change resets them.
    let stats = sink.property::<gst::Structure>("stats");
    let rendered = stats.get::<u64>("rendered").unwrap_or(0);
    let sink_dropped = stats.get::<u64>("dropped").unwrap_or(0);
    player.pause();
    settle(&player);
    let load_after = loadavg();
    stop(player, &events);
    let qos_dropped: u64 = events
        .all()
        .iter()
        .map(|e| match e {
            PlayerEvent::Late { dropped } => *dropped,
            _ => 0,
        })
        .sum();
    let span = match (first, last) {
        (Some(a), Some(b)) if b > a => (b - a).as_secs_f64(),
        _ => wall,
    };
    let fps = if frames > 1 {
        (frames - 1) as f64 / span
    } else {
        0.0
    };
    // Late: frames the sink or anything upstream dropped as late (QoS), of
    // the frames that reached the sink.
    let late_pct = if sync && frames > 0 {
        100.0 * qos_dropped as f64 / frames as f64
    } else {
        0.0
    };
    let report = serde_json::json!({
        "mode": mode,
        "outSize": out_size.map(|(w, h)| format!("{w}x{h}")),
        "seconds": (wall * 10.0).round() / 10.0,
        "frames": frames,
        "fps": (fps * 10.0).round() / 10.0,
        "rendered": rendered,
        "sinkDropped": sink_dropped,
        "qosDropped": qos_dropped,
        "arrivedOver20msLate": arrived_late.load(Ordering::SeqCst),
        "latePercent": (late_pct * 100.0).round() / 100.0,
        "cpuPercentOfOneCore": (100.0 * cpu_secs / wall).round(),
        "loadBefore": load_before,
        "loadAfter": load_after,
        "errors": events.errors(),
    });
    println!("QREC-MEASURE {report}");
}
