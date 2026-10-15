//! The fake backend's test hooks (docs/GUI.md G§8.4) on the real engine:
//! real GStreamer pipelines fed by `videotestsrc`/`audiotestsrc`, never the
//! portal, devices or a GPU. Each test sets [`EngineOptions::fake`] itself
//! (the variables are read by `EngineOptions::new`).

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use qrec_core::model::{AudioRole, AudioTrack, Canvas, Crop, Fit, Layer, Rect, Scene, VideoSource};
use qrec_core::plans::{AudioRecord, CapturePlan, LiveScene, VideoEncoder, VideoRecord};
use qrec_core::{Event, Fps, TakeId};

use super::fake::{FAKE_MIC, FakeHooks, FakePortal};
use super::{Backend, EngineOptions, GstCaptureEngine, SideEvent};
use crate::deps::CaptureEngine;
use crate::gst_init::testing::{assert_quiet_since, problems, serial};
use crate::msg::{Inbox, Msg};
use crate::paths::UserDirs;
use crate::testkit::TestEnv;

const WAIT: Duration = Duration::from_secs(20);

/// A fake-backend engine with `hooks` in a temp project.
struct Hooked {
    _env: TestEnv,
    project: PathBuf,
    engine: GstCaptureEngine,
    inbox: Inbox,
    rx: Receiver<Msg>,
    side: Receiver<SideEvent>,
    problems_before: u64,
}

impl Drop for Hooked {
    fn drop(&mut self) {
        self.engine.shutdown();
        if !std::thread::panicking() {
            assert_quiet_since(self.problems_before);
        }
    }
}

impl Hooked {
    fn new(hooks: FakeHooks) -> Self {
        let env = TestEnv::new();
        let project = env.empty_dir("swamp");
        fs::create_dir(project.join("takes")).unwrap();
        let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
        let (side_tx, side) = crossbeam_channel::unbounded();
        let mut opts = EngineOptions::new(&project, Backend::Fake, &dirs);
        opts.register_app_id = false;
        opts.side = Some(side_tx);
        opts.fake = hooks;
        let engine = GstCaptureEngine::spawn(opts).unwrap();
        let (inbox, rx) = Inbox::channel();
        Self {
            _env: env,
            project,
            engine,
            inbox,
            rx,
            side,
            problems_before: problems(),
        }
    }

    /// A take of the fake mic (and the screens named), its folder made.
    fn plan(&self, n: u32, screens: &[&str], eos_timeout_ms: u32) -> CapturePlan {
        let dir = format!("takes/{}", TakeId::numbered(n));
        fs::create_dir(self.project.join(&dir)).unwrap();
        CapturePlan {
            take: TakeId::numbered(n),
            dir: dir.clone(),
            log: format!("{dir}/recorder.log"),
            fps: Fps::new(30, 1),
            health_check_ms: 300,
            first_buffer_ms: 3000,
            eos_timeout_ms,
            video: screens
                .iter()
                .enumerate()
                .map(|(i, output)| screen(&format!("L{}", i + 1), output))
                .collect(),
            audio: vec![AudioRecord {
                id: "A1".parse().unwrap(),
                device: FAKE_MIC.into(),
                file: "A1-mic.mka".into(),
                sample_rate: 48_000,
            }],
        }
    }

    /// The next event `pred` matches, skipping others.
    fn wait_for(&self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + WAIT;
        let mut skipped = Vec::new();
        loop {
            match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Msg::Event(ev)) if pred(&ev) => return ev,
                Ok(Msg::Event(ev)) => skipped.push(ev),
                Ok(other) => panic!("unexpected message {other:?}"),
                Err(_) => panic!("no {what} within {WAIT:?}; got {skipped:?}"),
            }
        }
    }

    fn started(&self) {
        let ev = self.wait_for("CaptureStarted", |e| {
            matches!(
                e,
                Event::CaptureStarted { .. } | Event::CaptureFailed { .. }
            )
        });
        assert!(matches!(ev, Event::CaptureStarted { .. }), "{ev:?}");
    }

    /// Stops `take` and waits for its `CaptureStopped`: how long it took and
    /// whether EOS timed out.
    fn stop(&mut self, take: &TakeId) -> (Duration, bool) {
        let asked = Instant::now();
        self.engine.stop(take, false);
        match self.wait_for("CaptureStopped", |e| {
            matches!(e, Event::CaptureStopped { .. })
        }) {
            Event::CaptureStopped { timed_out, .. } => (asked.elapsed(), timed_out),
            _ => unreachable!(),
        }
    }
}

fn screen(id: &str, output: &str) -> VideoRecord {
    VideoRecord {
        id: id.parse().unwrap(),
        source: VideoSource::Screen {
            output: output.into(),
            label: format!("Fake {output}"),
        },
        file: format!("{id}-screen-{output}.mkv"),
        encoder: VideoEncoder::X264 {
            pass: "qual".into(),
            quantizer: 18,
            speed_preset: "veryfast".into(),
            tune: "zerolatency".into(),
        },
        key_int_max: 30,
        camera_mode: None,
    }
}

/// `QREC_FAKE_EOS_DELAY_MS`: a stopped take's files close that much later,
/// so the take stays finalizing; past its EOS timeout it ends `timed_out`.
#[test]
fn the_eos_delay_holds_a_stopped_take() {
    let _serial = serial();
    let delay = Duration::from_millis(900);
    let mut h = Hooked::new(FakeHooks {
        eos_delay: Some(delay),
        ..FakeHooks::default()
    });
    let take = h.plan(1, &[], 10_000);
    h.engine.start(take.clone(), h.inbox.clone());
    h.started();
    let (took, timed_out) = h.stop(&take.take);
    assert!(took >= delay, "stopped after {took:?}");
    assert!(took < delay + Duration::from_secs(5), "{took:?}");
    assert!(!timed_out);
    let log = fs::read_to_string(h.project.join(&take.log)).unwrap();
    assert!(log.contains("QREC_FAKE_EOS_DELAY_MS"), "{log}");

    // Longer than the take's EOS timeout: the files "didn't close".
    let take = h.plan(2, &[], 400);
    h.engine.start(take.clone(), h.inbox.clone());
    h.started();
    let (took, timed_out) = h.stop(&take.take);
    assert!(timed_out, "after {took:?}");
    assert!(took < delay, "the timeout ended it: {took:?}");
}

/// `QREC_FAKE_RECORD_OFFSET_FRAMES`: every `RecordClock` reads that many
/// frames more (4842 at 30 fps: S04's 00:02:41:12).
#[test]
fn the_record_offset_shows_in_record_clock() {
    let _serial = serial();
    let mut h = Hooked::new(FakeHooks {
        record_offset_frames: 4842,
        ..FakeHooks::default()
    });
    let take = h.plan(1, &[], 10_000);
    h.engine.start(take.clone(), h.inbox.clone());
    h.started();
    let clock = h.wait_for("RecordClock", |e| matches!(e, Event::RecordClock { .. }));
    let Event::RecordClock { elapsed_ns, .. } = clock else {
        unreachable!()
    };
    let offset = 161_400_000_000;
    assert!(
        (offset..offset + 5_000_000_000).contains(&elapsed_ns),
        "{elapsed_ns}"
    );
    h.stop(&take.take);
}

/// `QREC_FAKE_PORTAL=deny|cancel`: the fake screens are refused like the
/// real portal refuses them: a take that records one fails naming it, and
/// the live preview goes on without it.
#[test]
fn the_fake_portal_refuses_the_fake_screens() {
    let _serial = serial();
    for (portal, why) in [
        (FakePortal::Deny, "denied"),
        (FakePortal::Cancel, "cancelled"),
    ] {
        let mut h = Hooked::new(FakeHooks {
            portal: Some(portal),
            ..FakeHooks::default()
        });
        let take = h.plan(1, &["FAKE-1"], 10_000);
        h.engine.start(take.clone(), h.inbox.clone());
        let failed = h.wait_for("CaptureFailed", |e| {
            matches!(
                e,
                Event::CaptureFailed { .. } | Event::CaptureStarted { .. }
            )
        });
        let Event::CaptureFailed {
            take: failed_take,
            source,
            message,
            ..
        } = failed
        else {
            panic!("{portal:?}: {failed:?}");
        };
        assert_eq!(failed_take, Some(take.take.clone()));
        assert!(
            source.as_deref().is_some_and(|s| s.contains("L1")),
            "{source:?}"
        );
        assert!(
            message.contains("FAKE-1") && message.contains(why),
            "{message}"
        );

        // The live preview goes on without the refused screen.
        h.engine
            .set_live(Some(live_scene(&["FAKE-1"])), h.inbox.clone());
        let deadline = Instant::now() + WAIT;
        let refused = loop {
            match h
                .side
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(SideEvent::LiveError(text)) if text.contains("FAKE-1") => break text,
                Ok(_) => {}
                Err(_) => panic!("{portal:?}: no live error for FAKE-1"),
            }
        };
        assert!(refused.contains(why), "{refused}");
    }
}

/// A live scene of the fake screens named (each full canvas) and the mic.
fn live_scene(outputs: &[&str]) -> LiveScene {
    let layers = outputs
        .iter()
        .enumerate()
        .map(|(i, output)| Layer {
            id: format!("L{}", i + 1).parse().unwrap(),
            name: format!("Screen {output}"),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Screen {
                output: (*output).into(),
                label: format!("Fake {output}"),
            },
            rect: Rect::new(0, 0, 320, 180),
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        })
        .collect();
    LiveScene {
        canvas: Canvas {
            width: 320,
            height: 180,
            fps: Fps::new(30, 1),
            ..Canvas::default()
        },
        scene: Scene {
            layers,
            audio: vec![AudioTrack {
                id: "A1".parse().unwrap(),
                role: AudioRole::Mic,
                source: FAKE_MIC.into(),
                label: "Fake mic".into(),
                gain_db: 0.0,
                muted: false,
                sync_offset_ms: 0,
                missing: false,
            }],
        },
        preview: true,
    }
}
