//! Program playback sync (G§3.10): a pure state machine ([`SyncMachine`])
//! plus thin glue ([`ProgramSync`]) between the snapshots
//! (`runtime.playing`, `ui.playhead`, the plan), `LocalUi` (the trim scrub's
//! `trim_frame`, the bin preview's `take_preview`, the shuttle's
//! `playing_rate`) and the `ProgramPlayer` (load, seek, play with a rate,
//! pause). Owner: WP4.
//!
//! The protocol (G§3.10):
//!
//! 1. The plan is `without_tracks(plan_ges(project), ui.solo,
//!    ui.lane_mute)`. A change reloads the player at its position after
//!    [`RELOAD_DEBOUNCE_MS`] (newest wins; a load takes 0.35–1.2 s in debug
//!    builds, and the player coalesces the ones that pile up); while the
//!    stage shows Live the plan is only marked dirty and loads when Program
//!    comes back.
//! 2. `runtime.playing` false → true: `play(rate)` from `ui.playhead`, the
//!    rate being `LocalUi.playing_rate` when the GUI's own shuttle just set
//!    it and 1 otherwise (D-44: Space and CLI `qrec play` reset it). true →
//!    false: `pause()`, then `Seek{To(player frame)}`; the rate goes back
//!    to 1.
//! 3. While playing, `Seek{To(player frame)}` goes out once a second (an
//!    own seek); a playhead the GUI didn't set that way moves the player.
//! 4. Paused, a snapshot playhead the player isn't on moves it (`seek`),
//!    except the GUI's own seek during a ruler scrub, which `scrub` drives
//!    (D-40).
//! 5. `Ended` → `Pause`, then `Seek{End}` (`Seek{Start}` in reverse).
//! 6. `Error` → the error toast "Playback stopped"; the player stays paused.
//! 7. Shuttle rates come from the key router (D-44, `KeyRouter` pre-steps).
//! 8. The trim scrub (`LocalUi.trim_frame`, S10) and the bin preview
//!    (`LocalUi.take_preview`, D-17) load `plan_ges_take` (at the trimmed
//!    frame, or from 0); when they end the program plan loads again at
//!    `ui.playhead`.
//!
//! [`ProgramSync::install`] makes the window's one sync; the app hands it the
//! player's events ([`on_player_event`]) and the test channel asks it
//! whether a reload is still due ([`busy`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use qrec_core::ges_plan::{plan_ges, plan_ges_take, without_tracks};
use qrec_core::plans::GesTimeline;
use qrec_core::protocol::ToastLevel;
use qrec_core::protocol::command::SeekTarget;
use qrec_core::state::CanvasMode;
use qrec_core::time::{frame_ns, ns_to_frame_floor};
use qrec_core::timeline::total_frames;
use qrec_core::view::DragKind;
use qrec_core::view::notices::Notice;
use qrec_core::{Command, Fps, Frame, State, TakeId, TimeArg};
use qrec_shell::player::PlayerEvent;

use crate::Ctx;
use crate::guard;

/// How long the plan must hold still before the player reloads it.
pub const RELOAD_DEBOUNCE_MS: u64 = 100;
/// How often a playing player's frame becomes the playhead (an own seek).
pub const SEEK_EVERY_MS: u64 = 1000;
/// How long a rate set by the GUI's shuttle waits for playback to start.
pub const RATE_ARMED_MS: u64 = 1000;
/// A playhead this close to the player's frame doesn't move a playing
/// player (a pre-step's `Seek{To(player frame)}`).
const NEAR_FRAMES: Frame = 2;

/// What the player is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCmd {
    Load { plan: GesTimeline, at_ns: u64 },
    Seek(u64),
    Play(f64),
    Pause,
}

/// What the sync knows besides itself.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub state: &'a State,
    /// The program plan (`without_tracks(plan_ges(..), solo, lane_mute)`).
    pub plan: &'a GesTimeline,
    /// The trim scrub's take and take frame (S10).
    pub trim: Option<(TakeId, Frame)>,
    /// The bin preview's take, and whether it plays (D-17).
    pub take_preview: Option<(TakeId, bool)>,
    /// `LocalUi.playing_rate`.
    pub rate: f64,
    /// A ruler scrub is in progress (`LocalUi.drag == Scrub`).
    pub scrubbing: bool,
    /// The frame of the GUI's last `Seek{To}` (the echo rule's sent value).
    pub own_seek: Option<Frame>,
    /// The player's position.
    pub position_ns: u64,
    /// There is a player.
    pub has_player: bool,
    pub now_ms: u64,
}

impl Inputs<'_> {
    fn fps(&self) -> Fps {
        self.state.project.canvas.fps
    }

    fn ns(&self, frame: Frame) -> u64 {
        u64::try_from(frame_ns(
            i64::try_from(frame).unwrap_or(i64::MAX),
            self.fps(),
        ))
        .unwrap_or(0)
    }

    /// The player's frame on the timeline.
    fn player_frame(&self) -> Frame {
        let ns = i64::try_from(self.position_ns).unwrap_or(i64::MAX);
        let total = total_frames(&self.state.project.timeline.clips);
        (ns_to_frame_floor(ns, self.fps()).max(0) as Frame).min(total)
    }

    fn playhead(&self) -> Frame {
        let total = total_frames(&self.state.project.timeline.clips);
        self.state.ui.playhead.min(total)
    }
}

/// What the window does after a step.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Out {
    pub player: Vec<PlayerCmd>,
    pub dispatch: Vec<Command>,
    /// `LocalUi.playing`, when it changes.
    pub playing: Option<bool>,
    /// `LocalUi.playing_rate`, when it changes.
    pub rate: Option<f64>,
    pub notice: Option<Notice>,
    /// The bin preview played to its end (`take_preview.playing` = false).
    pub take_preview_ended: bool,
}

/// What the player holds.
#[derive(Debug, Clone, PartialEq)]
enum Content {
    Nothing,
    Program(GesTimeline),
    Take(TakeId),
}

/// The pure half of the sync (G§3.10).
#[derive(Debug)]
pub struct SyncMachine {
    content: Content,
    /// When the changed program plan loads (debounce).
    reload_due: Option<u64>,
    /// `runtime.playing` as last handled.
    playing: bool,
    /// The player plays (what it was told).
    told_playing: bool,
    /// The timeline frame a paused player was last put on.
    shown_frame: Option<Frame>,
    /// The playhead as last seen.
    seen_playhead: Option<Frame>,
    /// The take frame the trim scrub last showed.
    take_frame: Option<Frame>,
    /// The last once-a-second seek: (when, frame).
    auto_seek: Option<(u64, Frame)>,
    /// When the shuttle last set a rate (D-44).
    rate_armed_at: Option<u64>,
    /// Playback ended (`Ended`): the stop's seek is `Seek{End}`'s.
    ended: bool,
    /// The bin preview reached its end; a play starts it again from 0.
    take_ended: bool,
}

impl Default for SyncMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncMachine {
    pub fn new() -> Self {
        Self {
            content: Content::Nothing,
            reload_due: None,
            playing: false,
            told_playing: false,
            shown_frame: None,
            seen_playhead: None,
            take_frame: None,
            auto_seek: None,
            rate_armed_at: None,
            ended: false,
            take_ended: false,
        }
    }

    /// The GUI's shuttle set `LocalUi.playing_rate` (the key router's
    /// pre-step): the next start plays at it (D-44).
    pub fn arm_rate(&mut self, now_ms: u64) {
        self.rate_armed_at = Some(now_ms);
    }

    /// A reload is due (the test channel's `waitIdle`).
    pub fn busy(&self) -> bool {
        self.reload_due.is_some()
    }

    /// When [`sync`](Self::sync) must run again on its own: a due reload,
    /// the next once-a-second seek.
    pub fn deadline(&self) -> Option<u64> {
        let seek = (self.told_playing && matches!(self.content, Content::Program(_)))
            .then(|| self.auto_seek.map_or(0, |(at, _)| at + SEEK_EVERY_MS));
        match (self.reload_due, seek) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// One step: whatever the inputs ask of the player now.
    pub fn sync(&mut self, inp: &Inputs<'_>) -> Out {
        let mut out = Out::default();
        let now_playing = inp.state.runtime.playing;
        if !inp.has_player {
            self.content = Content::Nothing;
            self.playing = now_playing;
            return out;
        }
        if let Some((take, frame)) = &inp.trim {
            self.show_take(inp, take, Some(*frame), false, &mut out);
            self.playing = now_playing;
            return out;
        }
        if let Some((take, play)) = &inp.take_preview {
            self.show_take(inp, take, None, *play, &mut out);
            self.playing = now_playing;
            return out;
        }
        if inp.state.ui.canvas_mode != CanvasMode::Program {
            // Live: the plan only goes stale; it loads when Program comes
            // back. A player still going (a bin preview that ended in
            // Live) stops.
            if self.told_playing {
                out.player.push(PlayerCmd::Pause);
                self.told_playing = false;
                out.playing = Some(false);
            }
            self.reload_due = None;
            self.playing = now_playing;
            self.take_ended = false;
            return out;
        }
        let fresh = self.show_program(inp, &mut out);
        let started = now_playing && !self.playing;
        let stopped = !now_playing && self.playing;
        self.playing = now_playing;
        let playhead = inp.playhead();
        if started || (fresh && now_playing && !self.told_playing) {
            self.ended = false;
            let armed = self
                .rate_armed_at
                .is_some_and(|at| inp.now_ms.saturating_sub(at) <= RATE_ARMED_MS);
            let rate = if armed { inp.rate } else { 1.0 };
            if !armed && inp.rate != 1.0 {
                out.rate = Some(1.0);
            }
            self.rate_armed_at = None;
            if self.shown_frame != Some(playhead) {
                out.player.push(PlayerCmd::Seek(inp.ns(playhead)));
            }
            out.player.push(PlayerCmd::Play(rate));
            self.told_playing = true;
            out.playing = Some(true);
            self.auto_seek = Some((inp.now_ms, playhead));
            self.shown_frame = None;
        } else if stopped {
            if self.told_playing {
                out.player.push(PlayerCmd::Pause);
                self.told_playing = false;
                out.playing = Some(false);
            }
            if self.ended {
                // `Seek{End}` (or `Start`) went out with the `Pause`.
                self.shown_frame = None;
            } else {
                let frame = inp.player_frame();
                out.dispatch.push(seek_to(frame));
                self.shown_frame = Some(frame);
                self.seen_playhead = Some(frame);
            }
            if inp.rate != 1.0 {
                out.rate = Some(1.0);
            }
            self.ended = false;
            self.auto_seek = None;
        } else if now_playing && self.told_playing {
            // Playing: a playhead the GUI didn't set by its once-a-second
            // seek moves the player.
            let moved = self.seen_playhead != Some(playhead);
            let own = self.auto_seek.is_some_and(|(_, f)| f == playhead);
            if moved && !own && playhead.abs_diff(inp.player_frame()) > NEAR_FRAMES {
                out.player.push(PlayerCmd::Seek(inp.ns(playhead)));
                self.auto_seek = Some((inp.now_ms, playhead));
            } else if self
                .auto_seek
                .is_none_or(|(at, _)| inp.now_ms.saturating_sub(at) >= SEEK_EVERY_MS)
            {
                let frame = inp.player_frame();
                out.dispatch.push(seek_to(frame));
                self.auto_seek = Some((inp.now_ms, frame));
            }
        } else if !now_playing && self.shown_frame != Some(playhead) {
            // Paused: the player follows the playhead, but a ruler scrub
            // drives it itself (D-40).
            let scrubbed = inp.scrubbing && inp.own_seek == Some(playhead);
            if !scrubbed {
                out.player.push(PlayerCmd::Seek(inp.ns(playhead)));
            }
            self.shown_frame = Some(playhead);
        }
        self.seen_playhead = Some(playhead);
        out
    }

    /// The program plan in the player: loaded at once when the player held
    /// something else, after the debounce when only the plan changed.
    /// `true` when a load went out now.
    fn show_program(&mut self, inp: &Inputs<'_>, out: &mut Out) -> bool {
        let playhead = inp.playhead();
        match &self.content {
            Content::Program(loaded) if loaded == inp.plan => {
                self.reload_due = None;
                false
            }
            Content::Program(_) => {
                let due = *self
                    .reload_due
                    .get_or_insert(inp.now_ms + RELOAD_DEBOUNCE_MS);
                if inp.now_ms < due {
                    return false;
                }
                let at = if self.told_playing {
                    inp.position_ns
                } else {
                    inp.ns(playhead)
                };
                out.player.push(PlayerCmd::Load {
                    plan: inp.plan.clone(),
                    at_ns: at,
                });
                self.content = Content::Program(inp.plan.clone());
                self.reload_due = None;
                if !self.told_playing {
                    self.shown_frame = Some(playhead);
                }
                true
            }
            Content::Nothing | Content::Take(_) => {
                out.player.push(PlayerCmd::Load {
                    plan: inp.plan.clone(),
                    at_ns: inp.ns(playhead),
                });
                self.content = Content::Program(inp.plan.clone());
                self.reload_due = None;
                self.take_frame = None;
                self.take_ended = false;
                self.shown_frame = Some(playhead);
                // A bin preview that played: the player keeps playing
                // across a load, the program doesn't (unless it plays:
                // then `sync` starts it).
                if self.told_playing && !inp.state.runtime.playing {
                    out.player.push(PlayerCmd::Pause);
                    self.told_playing = false;
                    out.playing = Some(false);
                }
                true
            }
        }
    }

    /// A take in the player (the trim scrub at `frame`, or the bin preview
    /// from 0, playing when `play`).
    fn show_take(
        &mut self,
        inp: &Inputs<'_>,
        take: &TakeId,
        frame: Option<Frame>,
        play: bool,
        out: &mut Out,
    ) {
        self.reload_due = None;
        if self.content != Content::Take(take.clone()) {
            let plan = plan_ges_take(&inp.state.project, take);
            let at = frame.unwrap_or(0);
            out.player.push(PlayerCmd::Load {
                plan,
                at_ns: inp.ns(at),
            });
            self.content = Content::Take(take.clone());
            self.take_frame = Some(at);
            self.take_ended = false;
            self.shown_frame = None;
            if play {
                out.player.push(PlayerCmd::Play(1.0));
            } else if self.told_playing {
                out.player.push(PlayerCmd::Pause);
            }
            if self.told_playing != play {
                self.told_playing = play;
                out.playing = Some(play);
            }
            return;
        }
        if let Some(frame) = frame
            && self.take_frame != Some(frame)
        {
            out.player.push(PlayerCmd::Seek(inp.ns(frame)));
            self.take_frame = Some(frame);
        }
        if play != self.told_playing {
            if play {
                if self.take_ended {
                    out.player.push(PlayerCmd::Seek(0));
                    self.take_ended = false;
                }
                out.player.push(PlayerCmd::Play(1.0));
            } else {
                out.player.push(PlayerCmd::Pause);
            }
            self.told_playing = play;
            out.playing = Some(play);
        }
    }

    /// One of the player's events.
    pub fn on_event(&mut self, event: &PlayerEvent, inp: &Inputs<'_>) -> Out {
        let mut out = Out::default();
        match event {
            PlayerEvent::Ended { reverse } => match self.content {
                Content::Take(_) => {
                    if self.told_playing {
                        out.player.push(PlayerCmd::Pause);
                        self.told_playing = false;
                        out.playing = Some(false);
                    }
                    self.take_ended = true;
                    out.take_preview_ended = true;
                }
                Content::Program(_) if inp.state.runtime.playing => {
                    self.ended = true;
                    out.dispatch.push(Command::Pause);
                    out.dispatch.push(Command::Seek {
                        to: if *reverse {
                            SeekTarget::Start
                        } else {
                            SeekTarget::End
                        },
                    });
                }
                _ => {}
            },
            PlayerEvent::Error { message } => {
                out.notice = Some(
                    Notice::new(ToastLevel::Error, "Playback stopped").with_body(message.clone()),
                );
                if self.told_playing {
                    self.told_playing = false;
                    out.playing = Some(false);
                }
                if inp.state.runtime.playing {
                    self.ended = true;
                    out.dispatch.push(Command::Pause);
                }
            }
            PlayerEvent::Late { dropped } => {
                tracing::debug!("player: {dropped} late frames");
            }
            PlayerEvent::Loaded { .. } | PlayerEvent::Stopped => {}
        }
        out
    }
}

fn seek_to(frame: Frame) -> Command {
    Command::Seek {
        to: SeekTarget::To(TimeArg::frames(frame)),
    }
}

/// The frame of an own `Seek{To(f)}` (a plain frame count).
pub fn own_seek_frame(seek: Option<&SeekTarget>, fps: Fps) -> Option<Frame> {
    match seek? {
        SeekTarget::To(arg) => qrec_core::timeline::absolute_frame(arg, fps, "seek").ok(),
        _ => None,
    }
}

/// The program plan of `state` (G§3.10 step 1).
pub fn program_plan(state: &State) -> GesTimeline {
    without_tracks(
        plan_ges(&state.project),
        state.ui.solo.as_ref(),
        &state.ui.lane_mute,
    )
}

// --------------------------------------------------------------- the glue

thread_local! {
    static SYNC: RefCell<Option<Rc<ProgramSync>>> = const { RefCell::new(None) };
}

/// What the plan was computed from (it is recomputed when this changes).
#[derive(Debug, Clone, PartialEq)]
struct PlanKey {
    timeline: qrec_core::model::Timeline,
    takes_rev: u64,
    canvas: qrec_core::model::Canvas,
    solo: Option<qrec_core::TrackId>,
    lane_mute: std::collections::BTreeSet<qrec_core::TrackId>,
}

/// The window's one program sync.
pub struct ProgramSync {
    ctx: Ctx,
    machine: RefCell<SyncMachine>,
    plan: RefCell<Option<(PlanKey, Arc<GesTimeline>)>>,
    /// Bumped when `project.takes` changes (the plan reads the take scenes).
    takes_rev: Cell<u64>,
    timer: RefCell<Option<glib::SourceId>>,
    timer_at: Cell<Option<u64>>,
    /// The rate the sync wrote itself (its subscription ignores it).
    own_rate: Cell<Option<f64>>,
    started: Instant,
    pending: Cell<bool>,
}

impl ProgramSync {
    /// Makes the sync and follows the store (once per window).
    pub fn install(ctx: &Ctx) -> Rc<ProgramSync> {
        let sync = Rc::new(ProgramSync {
            ctx: ctx.clone(),
            machine: RefCell::new(SyncMachine::new()),
            plan: RefCell::new(None),
            takes_rev: Cell::new(0),
            timer: RefCell::new(None),
            timer_at: Cell::new(None),
            own_rate: Cell::new(None),
            started: Instant::now(),
            pending: Cell::new(false),
        });
        let weak = Rc::downgrade(&sync);
        ctx.store
            .subscribe(move |change| {
                if let Some(sync) = weak.upgrade() {
                    if change.takes() {
                        sync.takes_rev.set(sync.takes_rev.get() + 1);
                    }
                    sync.schedule();
                }
            })
            .detach();
        let weak = Rc::downgrade(&sync);
        let last = Rc::new(RefCell::new((false, 1.0f64)));
        ctx.store
            .subscribe_local(move |local| {
                let Some(sync) = weak.upgrade() else { return };
                let mut seen = last.borrow_mut();
                let was_open = seen.0;
                if local.playing_rate != seen.1 {
                    let own = sync.own_rate.take() == Some(local.playing_rate);
                    if !own && !local.playing {
                        let now = sync.now_ms();
                        sync.machine.borrow_mut().arm_rate(now);
                    }
                }
                *seen = (local.is_open(), local.playing_rate);
                drop(seen);
                if local.is_open() && !was_open {
                    // The player is spawned right after the open is told
                    // (startup step 8): look once that ran.
                    let weak = Rc::downgrade(&sync);
                    guard::idle_local_once("program sync: the player", move || {
                        if let Some(sync) = weak.upgrade() {
                            sync.run();
                        }
                    });
                }
                sync.schedule();
            })
            .detach();
        SYNC.with(|s| *s.borrow_mut() = Some(Rc::clone(&sync)));
        sync
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Runs a step soon (after the current dispatch: changes come in
    /// bursts, a snapshot and a local change together).
    fn schedule(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        guard::idle_local_once("program sync: a step", move || {
            if let Some(sync) = weak.upgrade() {
                sync.pending.set(false);
                sync.run();
            }
        });
    }

    /// The program plan, recomputed only when what it reads changed.
    fn plan(&self, state: &State) -> Arc<GesTimeline> {
        let key = PlanKey {
            timeline: state.project.timeline.clone(),
            takes_rev: self.takes_rev.get(),
            canvas: state.project.canvas.clone(),
            solo: state.ui.solo.clone(),
            lane_mute: state.ui.lane_mute.clone(),
        };
        if let Some((k, plan)) = &*self.plan.borrow()
            && *k == key
        {
            return Arc::clone(plan);
        }
        let plan = Arc::new(program_plan(state));
        *self.plan.borrow_mut() = Some((key, Arc::clone(&plan)));
        plan
    }

    fn inputs<'a>(&self, state: &'a State, plan: &'a GesTimeline) -> Inputs<'a> {
        let local = self.ctx.store.local();
        let player = self.ctx.media.player();
        let fps = state.project.canvas.fps;
        Inputs {
            state,
            plan,
            trim: local.trim_frame.as_ref().map(|t| (t.take.clone(), t.frame)),
            take_preview: local
                .take_preview
                .as_ref()
                .map(|t| (t.take.clone(), t.playing)),
            rate: local.playing_rate,
            scrubbing: local.drag == Some(DragKind::Scrub),
            own_seek: own_seek_frame(self.ctx.store.sent().seek.as_ref(), fps),
            position_ns: player.as_ref().map_or(0, |p| p.position_ns()),
            has_player: player.is_some() && local.is_open(),
            now_ms: self.now_ms(),
        }
    }

    /// One step of the machine, carried out.
    fn run(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let plan = self.plan(&state);
        let out = {
            let inp = self.inputs(&state, &plan);
            self.machine.borrow_mut().sync(&inp)
        };
        self.apply(out);
        self.arm_timer();
    }

    /// A player event (from the queue).
    fn event(self: &Rc<Self>, event: &PlayerEvent) {
        let state = self.ctx.store.state();
        let plan = self.plan(&state);
        let out = {
            let inp = self.inputs(&state, &plan);
            self.machine.borrow_mut().on_event(event, &inp)
        };
        self.apply(out);
        self.arm_timer();
    }

    fn apply(&self, out: Out) {
        if let Some(player) = self.ctx.media.player() {
            for cmd in out.player {
                match cmd {
                    PlayerCmd::Load { plan, at_ns } => {
                        tracing::debug!(
                            "program sync: load {} clips at {at_ns} ns",
                            plan.video.len()
                        );
                        player.load(plan, at_ns);
                    }
                    PlayerCmd::Seek(ns) => player.seek(ns),
                    PlayerCmd::Play(rate) => player.play(rate),
                    PlayerCmd::Pause => player.pause(),
                }
            }
        }
        for cmd in out.dispatch {
            self.ctx.store.dispatch(cmd);
        }
        if let Some(notice) = out.notice {
            self.ctx.store.notify(notice);
        }
        let playing = out.playing;
        let rate = out.rate;
        if let Some(rate) = rate {
            self.own_rate.set(Some(rate));
        }
        if playing.is_some() || rate.is_some() || out.take_preview_ended {
            let ended = out.take_preview_ended;
            self.ctx.store.update_local(move |l| {
                if let Some(playing) = playing {
                    l.playing = playing;
                }
                if let Some(rate) = rate {
                    l.playing_rate = rate;
                }
                if ended && let Some(preview) = &mut l.take_preview {
                    preview.playing = false;
                }
            });
        }
    }

    /// One timeout to the machine's next deadline.
    fn arm_timer(self: &Rc<Self>) {
        let deadline = self.machine.borrow().deadline();
        if deadline == self.timer_at.get() && self.timer.borrow().is_some() {
            return;
        }
        if let Some(old) = self.timer.borrow_mut().take() {
            old.remove();
        }
        self.timer_at.set(deadline);
        let Some(at) = deadline else { return };
        let wait = at.saturating_sub(self.now_ms()).max(1);
        let weak = Rc::downgrade(self);
        let id = guard::timeout_local_once(
            Duration::from_millis(wait),
            "program sync: the timer",
            move || {
                if let Some(sync) = weak.upgrade() {
                    sync.timer.borrow_mut().take();
                    sync.timer_at.set(None);
                    sync.run();
                }
            },
        );
        *self.timer.borrow_mut() = Some(id);
    }
}

/// A player event, from the app's queue drain.
pub fn on_player_event(event: &PlayerEvent) {
    let sync = SYNC.with(|s| s.borrow().clone());
    if let Some(sync) = sync {
        sync.event(event);
    }
}

/// Why the program picture isn't settled (the test channel's `waitIdle`):
/// a reload is still due.
pub fn busy() -> Option<String> {
    SYNC.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|sync| sync.machine.borrow().busy() || sync.pending.get())
            .map(|_| "the program sync".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use qrec_core::ids::ClipId;
    use qrec_core::testkit;

    use super::*;

    /// The design state in Program mode, paused at 2437.
    fn program() -> State {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        state
    }

    struct Rig {
        machine: SyncMachine,
        state: State,
        plan: GesTimeline,
        trim: Option<(TakeId, Frame)>,
        take_preview: Option<(TakeId, bool)>,
        rate: f64,
        scrubbing: bool,
        own_seek: Option<Frame>,
        position: Frame,
        now: u64,
    }

    impl Rig {
        fn new(state: State) -> Self {
            let plan = program_plan(&state);
            Self {
                machine: SyncMachine::new(),
                state,
                plan,
                trim: None,
                take_preview: None,
                rate: 1.0,
                scrubbing: false,
                own_seek: None,
                position: 0,
                now: 1000,
            }
        }

        fn inputs(&self) -> Inputs<'_> {
            let fps = self.state.project.canvas.fps;
            Inputs {
                state: &self.state,
                plan: &self.plan,
                trim: self.trim.clone(),
                take_preview: self.take_preview.clone(),
                rate: self.rate,
                scrubbing: self.scrubbing,
                own_seek: self.own_seek,
                position_ns: frame_ns(self.position as i64, fps) as u64,
                has_player: true,
                now_ms: self.now,
            }
        }

        fn sync(&mut self) -> Out {
            // The machine is a field of the rig: out while it runs.
            let mut machine = std::mem::take(&mut self.machine);
            let out = machine.sync(&self.inputs());
            self.machine = machine;
            out
        }

        fn event(&mut self, event: PlayerEvent) -> Out {
            let mut machine = std::mem::take(&mut self.machine);
            let out = machine.on_event(&event, &self.inputs());
            self.machine = machine;
            out
        }

        fn ns(&self, frame: Frame) -> u64 {
            frame_ns(frame as i64, self.state.project.canvas.fps) as u64
        }

        /// Loaded and settled at the playhead.
        fn loaded(mut self) -> Self {
            let out = self.sync();
            assert!(
                matches!(out.player[..], [PlayerCmd::Load { .. }]),
                "{out:?}"
            );
            self.position = self.state.ui.playhead;
            assert_eq!(self.sync(), Out::default());
            self
        }

        fn play(&mut self) -> Out {
            self.state.runtime.playing = true;
            self.sync()
        }

        fn pause(&mut self) -> Out {
            self.state.runtime.playing = false;
            self.sync()
        }
    }

    fn seek_cmd(frame: Frame) -> Command {
        seek_to(frame)
    }

    #[test]
    fn program_loads_at_the_playhead_and_live_only_marks_it_dirty() {
        let mut rig = Rig::new(testkit::design_state());
        // Live: nothing loads.
        assert_eq!(rig.sync(), Out::default());
        rig.state.ui.canvas_mode = CanvasMode::Program;
        let out = rig.sync();
        assert_eq!(
            out.player,
            [PlayerCmd::Load {
                plan: rig.plan.clone(),
                at_ns: rig.ns(2437)
            }]
        );
        // A plan change while Live waits for Program.
        rig.state.ui.canvas_mode = CanvasMode::Live;
        rig.state.project.timeline.clips.pop();
        rig.plan = program_plan(&rig.state);
        assert_eq!(rig.sync(), Out::default());
        assert!(!rig.machine.busy());
        rig.state.ui.canvas_mode = CanvasMode::Program;
        // Back in Program: the changed plan loads after the debounce.
        assert_eq!(rig.sync().player, []);
        assert!(rig.machine.busy());
        assert_eq!(rig.machine.deadline(), Some(rig.now + RELOAD_DEBOUNCE_MS));
        rig.now += RELOAD_DEBOUNCE_MS;
        let out = rig.sync();
        assert!(matches!(&out.player[..], [PlayerCmd::Load { plan, .. }] if *plan == rig.plan));
        assert!(!rig.machine.busy());
    }

    #[test]
    fn plan_changes_reload_at_the_players_position_once_they_settle() {
        let mut rig = Rig::new(program()).loaded();
        // Two edits 50 ms apart: one load, the newest plan.
        rig.state.project.timeline.clips.pop();
        rig.plan = program_plan(&rig.state);
        assert_eq!(rig.sync().player, []);
        rig.now += 50;
        rig.state.ui.solo = Some("A1".parse().unwrap());
        rig.plan = program_plan(&rig.state);
        assert_eq!(rig.sync().player, []);
        rig.now += RELOAD_DEBOUNCE_MS;
        let out = rig.sync();
        assert_eq!(
            out.player,
            [PlayerCmd::Load {
                plan: rig.plan.clone(),
                at_ns: rig.ns(2437)
            }]
        );
        assert!(
            rig.plan.audio.iter().all(|a| a.track.as_str() == "A1"),
            "solo"
        );
    }

    #[test]
    fn playing_seeks_once_a_second_and_stopping_parks_the_playhead() {
        let mut rig = Rig::new(program()).loaded();
        let out = rig.play();
        assert_eq!(out.player, [PlayerCmd::Play(1.0)]);
        assert_eq!(out.playing, Some(true));
        assert_eq!(rig.machine.deadline(), Some(rig.now + SEEK_EVERY_MS));
        // Half a second later nothing; a second later an own seek.
        rig.position = 2452;
        rig.now += 500;
        assert_eq!(rig.sync(), Out::default());
        rig.position = 2467;
        rig.now += 500;
        let out = rig.sync();
        assert_eq!(out.dispatch, [seek_cmd(2467)]);
        assert_eq!(out.player, []);
        // Its snapshot doesn't move the player.
        rig.state.ui.playhead = 2467;
        rig.position = 2470;
        rig.now += 10;
        assert_eq!(rig.sync(), Out::default());
        // An external seek (the CLI's) moves it.
        rig.state.ui.playhead = 100;
        let out = rig.sync();
        assert_eq!(out.player, [PlayerCmd::Seek(rig.ns(100))]);
        rig.position = 130;
        // Stop: pause, then the playhead at the player's frame.
        let out = rig.pause();
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(out.dispatch, [seek_cmd(130)]);
        assert_eq!(out.playing, Some(false));
        // That seek's snapshot doesn't seek again.
        rig.state.ui.playhead = 130;
        assert_eq!(rig.sync(), Out::default());
    }

    #[test]
    fn a_paused_player_follows_external_seeks_but_not_its_own_scrub() {
        let mut rig = Rig::new(program()).loaded();
        rig.state.ui.playhead = 3000;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(3000))]);
        rig.position = 3000;
        assert_eq!(rig.sync(), Out::default());
        // A ruler scrub: the GUI's own throttled seeks don't fight it.
        rig.scrubbing = true;
        rig.position = 3100;
        rig.own_seek = Some(3050);
        rig.state.ui.playhead = 3050;
        assert_eq!(rig.sync().player, []);
        // The release's seek after the scrub: the player is there already.
        rig.scrubbing = false;
        rig.own_seek = Some(3100);
        rig.state.ui.playhead = 3100;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(3100))]);
    }

    #[test]
    fn ended_pauses_and_parks_at_the_end() {
        let mut rig = Rig::new(program()).loaded();
        rig.play();
        rig.position = 7391;
        let out = rig.event(PlayerEvent::Ended { reverse: false });
        assert_eq!(
            out.dispatch,
            [
                Command::Pause,
                Command::Seek {
                    to: SeekTarget::End
                }
            ]
        );
        // The pause's snapshot: no second seek.
        let out = rig.pause();
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(out.dispatch, []);
        // Seek{End}'s snapshot puts the paused player on the last frame.
        rig.state.ui.playhead = 7391;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(7391))]);
        // Reverse ends at the start.
        rig.play();
        let out = rig.event(PlayerEvent::Ended { reverse: true });
        assert_eq!(
            out.dispatch[1],
            Command::Seek {
                to: SeekTarget::Start
            }
        );
    }

    #[test]
    fn errors_toast_and_pause() {
        let mut rig = Rig::new(program()).loaded();
        rig.play();
        let out = rig.event(PlayerEvent::Error {
            message: "no such file".into(),
        });
        let notice = out.notice.expect("a toast");
        assert_eq!(notice.title, "Playback stopped");
        assert_eq!(notice.body.as_deref(), Some("no such file"));
        assert_eq!(out.dispatch, [Command::Pause]);
        assert_eq!(out.playing, Some(false));
        let out = rig.pause();
        assert_eq!(out.dispatch, [], "no seek after a failed playback");
    }

    /// D-44: the GUI's shuttle sets the rate before playback starts; Space
    /// and CLI `qrec play` play at 1 and reset a stale rate; a stop resets
    /// it.
    #[test]
    fn shuttle_rates_and_the_reset() {
        let mut rig = Rig::new(program()).loaded();
        // L L L before playing: the router set 1 → playback at 1.
        rig.rate = 1.0;
        rig.machine.arm_rate(rig.now);
        assert_eq!(rig.play().player, [PlayerCmd::Play(1.0)]);
        // While playing the router tells the player itself (2×, 4×): the
        // sync doesn't touch it.
        rig.rate = 4.0;
        rig.now += 100;
        assert_eq!(rig.sync().player, []);
        // K: the stop resets the rate.
        let out = rig.pause();
        assert_eq!(out.rate, Some(1.0));
        rig.rate = 1.0;
        // J from rest: the router armed −1.
        rig.rate = -1.0;
        rig.machine.arm_rate(rig.now);
        let out = rig.play();
        assert_eq!(out.player, [PlayerCmd::Play(-1.0)]);
        rig.pause();
        // A stale −2 that no shuttle armed (reverse at 0 never started):
        // `qrec play` plays at 1 and resets it.
        rig.rate = -2.0;
        rig.machine.arm_rate(rig.now);
        rig.now += RATE_ARMED_MS + 1;
        let out = rig.play();
        assert_eq!(out.player, [PlayerCmd::Play(1.0)]);
        assert_eq!(out.rate, Some(1.0));
    }

    /// S10 and D-17: a take loads at the trimmed frame (or from 0), its
    /// frames seek, and the program plan comes back at the playhead.
    #[test]
    fn trim_and_bin_previews_load_and_restore() {
        let mut rig = Rig::new(program()).loaded();
        let take5 = TakeId::numbered(5);
        rig.trim = Some((take5.clone(), 41));
        let out = rig.sync();
        assert_eq!(
            out.player,
            [PlayerCmd::Load {
                plan: plan_ges_take(&rig.state.project, &take5),
                at_ns: rig.ns(41)
            }]
        );
        rig.trim = Some((take5.clone(), 44));
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(44))]);
        assert_eq!(rig.sync().player, []);
        rig.trim = None;
        let out = rig.sync();
        assert_eq!(
            out.player,
            [PlayerCmd::Load {
                plan: rig.plan.clone(),
                at_ns: rig.ns(2437)
            }]
        );
        // The bin preview plays take 4 from 0, toggles, ends.
        let take4 = TakeId::numbered(4);
        rig.take_preview = Some((take4.clone(), true));
        let out = rig.sync();
        assert_eq!(
            out.player,
            [
                PlayerCmd::Load {
                    plan: plan_ges_take(&rig.state.project, &take4),
                    at_ns: 0
                },
                PlayerCmd::Play(1.0)
            ]
        );
        assert_eq!(out.playing, Some(true));
        rig.take_preview = Some((take4.clone(), false));
        assert_eq!(rig.sync().player, [PlayerCmd::Pause]);
        rig.take_preview = Some((take4.clone(), true));
        assert_eq!(rig.sync().player, [PlayerCmd::Play(1.0)]);
        let out = rig.event(PlayerEvent::Ended { reverse: false });
        assert!(out.take_preview_ended);
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(out.dispatch, [], "the program's playhead stays");
        // Played again after its end: from the start.
        rig.take_preview = Some((take4.clone(), false));
        rig.sync();
        rig.take_preview = Some((take4, true));
        assert_eq!(
            rig.sync().player,
            [PlayerCmd::Seek(0), PlayerCmd::Play(1.0)]
        );
        // The preview ends: back to the program, paused, at the playhead.
        rig.take_preview = None;
        let out = rig.sync();
        assert_eq!(
            out.player,
            [
                PlayerCmd::Load {
                    plan: rig.plan.clone(),
                    at_ns: rig.ns(2437)
                },
                PlayerCmd::Pause
            ]
        );
        assert_eq!(out.playing, Some(false));
    }

    #[test]
    fn without_a_player_nothing_happens() {
        let mut rig = Rig::new(program());
        let mut machine = std::mem::take(&mut rig.machine);
        let mut inp = rig.inputs();
        inp.has_player = false;
        assert_eq!(machine.sync(&inp), Out::default());
        rig.machine = machine;
        // The player comes: the first load.
        assert!(matches!(rig.sync().player[..], [PlayerCmd::Load { .. }]));
    }

    #[test]
    fn own_seeks_are_plain_frames() {
        let fps = Fps::new(30, 1);
        assert_eq!(
            own_seek_frame(Some(&SeekTarget::To(TimeArg::frames(40))), fps),
            Some(40)
        );
        assert_eq!(own_seek_frame(Some(&SeekTarget::End), fps), None);
        assert_eq!(own_seek_frame(None, fps), None);
        // The selection doesn't change the plan; solo does.
        let mut state = program();
        let plan = program_plan(&state);
        state.ui.selection = vec![ClipId::from_raw("c2d0")];
        assert_eq!(program_plan(&state), plan);
    }
}
