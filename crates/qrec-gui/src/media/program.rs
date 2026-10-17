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
//!    to 1. Leaving Program while playing is that stop too (R21).
//! 3. While playing, `Seek{To(player frame)}` goes out once a second (an
//!    own seek); a playhead the GUI didn't set that way (nor by a key's
//!    pre-step seek to the player's frame, R22) moves the player.
//!    `LocalUi.program_playing` says the program plays in the player (not
//!    a take; and only once the program's load after a take landed, the
//!    player idle, since until then its position is the take's): what the
//!    timeline, the transport and the key router follow (R00, R16).
//! 4. Paused, a snapshot playhead the player isn't on moves it (`seek`),
//!    except the GUI's own seek during a ruler scrub, which `scrub` drives
//!    (D-40).
//! 5. `Ended` → `Pause`, then `Seek{End}` (`Seek{Start}` in reverse).
//! 6. `Error` → the error toast "Playback stopped"; the player stays paused
//!    (a bin preview's `playing` too: the next play starts it from 0).
//! 7. Shuttle rates come from the key router (D-44, `KeyRouter` pre-steps).
//! 8. The trim scrub (`LocalUi.trim_frame`, S10) and the bin preview
//!    (`LocalUi.take_preview`, D-17) load `plan_ges_take` (at the trimmed
//!    frame, or from 0); when they end the program plan loads again at
//!    `ui.playhead`. A bin preview that takes over a playing program
//!    pauses it and parks its playhead at the frame the player showed
//!    (`Pause`, `Seek{To(f)}`, as a stop does: not at the last
//!    once-a-second seek), a seek the preview takes as its own until its
//!    reply (`TakePreview::parking`: it doesn't end the preview).
//! 9. A ruler drag (D-40) scrubs through the sync ([`scrub`], the
//!    timeline's): the paused program player scrubs to every frame; the
//!    drag's own throttled seeks never move it, and after the drag the
//!    sync waits (up to [`SCRUB_ECHO_MS`]) for the snapshots to reach the
//!    last scrubbed frame instead of seeking back to an older one. While
//!    playing, or with a take or nothing in the player, the drag's seeks
//!    move the player instead.
//!
//! [`ProgramSync::install`] makes the window's one sync; the app hands it the
//! player's events ([`on_player_event`]) and the test channel asks it
//! whether a reload is still due ([`busy`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
/// player (a frame step while playing, say); a key's pre-step seek to the
/// player's frame is own whatever the distance
/// ([`SyncMachine::note_own_seek`]).
const NEAR_FRAMES: Frame = 2;
/// How long after a ruler drag the sync waits for the snapshots to reach
/// its last scrubbed frame (the echo of the drag's last seek) before it
/// follows the playhead again (a refused seek never echoes).
pub const SCRUB_ECHO_MS: u64 = 1000;
/// How often the sync looks whether the program's load after a take
/// landed (the player is idle): until then its position is the take's.
pub const SETTLE_POLL_MS: u64 = 50;

/// What the player is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCmd {
    Load {
        plan: GesTimeline,
        at_ns: u64,
    },
    Seek(u64),
    /// A seek plus 60 ms of audio (a ruler drag, D-40).
    Scrub(u64),
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
    /// The player has nothing to load or seek, queued or in flight
    /// (`ProgramPlayer::idle`): its position is the last load's or seek's.
    pub player_idle: bool,
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
    /// `LocalUi.playing` (the player plays, the program or a take), when it
    /// changes.
    pub playing: Option<bool>,
    /// `LocalUi.program_playing`, when it changes: the program plays in the
    /// player and the player's position is the program's (not a take's,
    /// nor while the program's load after one is on its way).
    pub program_playing: Option<bool>,
    /// `LocalUi.playing_rate`, when it changes.
    pub rate: Option<f64>,
    pub notice: Option<Notice>,
    /// The bin preview played to its end or failed
    /// (`take_preview.playing` = false).
    pub take_preview_ended: bool,
    /// The bin preview took the player from the playing program: its
    /// playhead parks at this frame, the one the player showed (step 8,
    /// `Seek{To(frame)}` after the `Pause`), a seek the preview takes as its
    /// own (`TakePreview::parking`: it doesn't end it).
    pub park: Option<Frame>,
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
    /// The frame a ruler drag last scrubbed the player to (D-40), until
    /// the snapshots reach it.
    scrubbed: Option<Frame>,
    /// When the drag that scrubbed ended (the echo wait's start).
    scrub_ended_at: Option<u64>,
    /// The program's load after a take (or nothing) hasn't landed: the
    /// player's position isn't the program's yet (it is the take's).
    settling: bool,
    /// When the sync looks again whether that load landed.
    settle_check: Option<u64>,
    /// `LocalUi.program_playing` as last told.
    program_playing: bool,
    /// The bin preview paused the program's playback (once per preview).
    preview_paused: bool,
    /// The frames the key router took from the player lately (its
    /// pre-step seeks): (when, frame).
    key_seeks: Vec<(u64, Frame)>,
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
            scrubbed: None,
            scrub_ended_at: None,
            settling: false,
            settle_check: None,
            program_playing: false,
            preview_paused: false,
            key_seeks: Vec::new(),
        }
    }

    /// The GUI's shuttle set `LocalUi.playing_rate` (the key router's
    /// pre-step): the next start plays at it (D-44).
    pub fn arm_rate(&mut self, now_ms: u64) {
        self.rate_armed_at = Some(now_ms);
    }

    /// The key router took the player's `frame` for the playhead (its
    /// pre-step `Seek{To(frame)}`, G§3.8): a playhead at that frame within
    /// [`SEEK_EVERY_MS`] is the GUI's own and doesn't move a playing
    /// player (R22: at 4×, or stepping in reverse, the player is past it
    /// by the time the snapshot comes).
    pub fn note_own_seek(&mut self, now_ms: u64, frame: Frame) {
        self.key_seeks
            .retain(|(at, _)| now_ms.saturating_sub(*at) < SEEK_EVERY_MS);
        self.key_seeks.push((now_ms, frame));
    }

    /// The window closes its host next (`Store::before_close`, G§3.6): a
    /// program playing in the player parks the playhead at its frame now,
    /// as a stop does. The once-a-second seek still to come went out after
    /// the close, refused, and the reopened project showed the playhead up
    /// to a second earlier (A1's audit). Nothing for a paused program, a
    /// take in the player, or a program whose load after a take hasn't
    /// landed (its position is still the take's).
    pub fn on_close(&mut self, inp: &Inputs<'_>) -> Option<Command> {
        let playing = inp.has_player
            && self.told_playing
            && matches!(self.content, Content::Program(_))
            && !self.settling;
        if !playing {
            return None;
        }
        let frame = inp.player_frame();
        self.auto_seek = Some((inp.now_ms, frame));
        Some(seek_to(frame))
    }

    /// A reload is due (the test channel's `waitIdle`).
    pub fn busy(&self) -> bool {
        self.reload_due.is_some()
    }

    /// When [`sync`](Self::sync) must run again on its own: a due reload,
    /// the next once-a-second seek, the end of a scrub's echo wait, the
    /// next look whether the program's load after a take landed.
    pub fn deadline(&self) -> Option<u64> {
        let seek =
            (self.told_playing && matches!(self.content, Content::Program(_)) && !self.settling)
                .then(|| self.auto_seek.map_or(0, |(at, _)| at + SEEK_EVERY_MS));
        let echo = self
            .scrubbed
            .and(self.scrub_ended_at)
            .map(|at| at + SCRUB_ECHO_MS);
        [self.reload_due, seek, echo, self.settle_check]
            .into_iter()
            .flatten()
            .min()
    }

    /// The program's load after a take landed once the player is idle
    /// (its position is the load's, or where it played on from). Checked
    /// before a step's own commands: a load this step sends isn't there yet.
    fn check_settled(&mut self, inp: &Inputs<'_>) {
        if self.settling && inp.player_idle {
            self.settling = false;
        }
    }

    /// `LocalUi.program_playing` (G§3.10 step 3): the program plays in the
    /// player, and the player's position is the program's.
    fn report(&mut self, inp: &Inputs<'_>, out: &mut Out) {
        let program =
            self.told_playing && matches!(self.content, Content::Program(_)) && !self.settling;
        if program != self.program_playing {
            self.program_playing = program;
            out.program_playing = Some(program);
        }
        self.settle_check = self.settling.then_some(inp.now_ms + SETTLE_POLL_MS);
    }

    /// A ruler drag's motion (D-40, the timeline's): the paused program
    /// player scrubs to `frame`. While playing, or with a take or nothing
    /// in the player (or in Live), nothing: the drag's seeks move it.
    pub fn scrub(&mut self, frame: Frame, inp: &Inputs<'_>) -> Out {
        let mut out = Out::default();
        let program = matches!(self.content, Content::Program(_))
            && inp.state.ui.canvas_mode == CanvasMode::Program
            && inp.trim.is_none()
            && inp.take_preview.is_none();
        if !inp.has_player || !program || self.told_playing || inp.state.runtime.playing {
            return out;
        }
        let total = total_frames(&inp.state.project.timeline.clips);
        let frame = frame.min(total);
        out.player.push(PlayerCmd::Scrub(inp.ns(frame)));
        self.scrubbed = Some(frame);
        self.scrub_ended_at = None;
        self.shown_frame = Some(frame);
        out
    }

    /// A ruler scrub holds the paused player (no seek to `playhead`): while
    /// its drag runs (the drag's own seeks lag behind its scrubs), and
    /// after it while the snapshots haven't reached its last frame yet,
    /// the GUI's last seek being that frame's, for at most
    /// [`SCRUB_ECHO_MS`].
    fn scrub_holds(&mut self, inp: &Inputs<'_>, playhead: Frame) -> bool {
        let Some(scrubbed) = self.scrubbed else {
            return false;
        };
        if inp.scrubbing {
            self.scrub_ended_at = None;
            return true;
        }
        let ended = *self.scrub_ended_at.get_or_insert(inp.now_ms);
        let waiting = playhead != scrubbed
            && inp.own_seek == Some(scrubbed)
            && inp.now_ms.saturating_sub(ended) < SCRUB_ECHO_MS;
        if !waiting {
            self.scrubbed = None;
            self.scrub_ended_at = None;
        }
        waiting
    }

    /// One step: whatever the inputs ask of the player now.
    pub fn sync(&mut self, inp: &Inputs<'_>) -> Out {
        self.check_settled(inp);
        let mut out = self.step(inp);
        self.report(inp, &mut out);
        out
    }

    fn step(&mut self, inp: &Inputs<'_>) -> Out {
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
            // The preview takes the player: the program doesn't play under
            // it (R16), and its playhead parks at the frame the player
            // showed, as a stop does (step 2), not at the last once-a-second
            // seek (read before the take loads: the position is still the
            // program's). The park is the preview's own: it doesn't end it.
            if now_playing && !self.preview_paused {
                out.dispatch.push(Command::Pause);
                let program = self.told_playing
                    && matches!(self.content, Content::Program(_))
                    && !self.settling;
                if program && inp.player_frame() != inp.playhead() {
                    out.park = Some(inp.player_frame());
                }
                self.preview_paused = true;
                self.auto_seek = None;
                if inp.rate != 1.0 {
                    out.rate = Some(1.0);
                }
            }
            self.show_take(inp, take, None, *play, &mut out);
            self.playing = now_playing;
            return out;
        }
        self.preview_paused = false;
        if inp.state.ui.canvas_mode != CanvasMode::Program {
            // Live: the plan only goes stale; it loads when Program comes
            // back. A program that played stops as a pause does (step 2:
            // the playhead parks at the player's frame, R21); a player
            // still going (a bin preview that ended in Live) stops.
            if self.playing && !now_playing && matches!(self.content, Content::Program(_)) {
                self.stop(inp, &mut out);
            } else if self.told_playing {
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
            self.scrubbed = None;
            self.scrub_ended_at = None;
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
            self.stop(inp, &mut out);
        } else if now_playing && self.told_playing && self.settling {
            // The program's load after a take is on its way: the player's
            // position is still the take's, so no own seek yet, and a
            // playhead that moves meanwhile moves the player once it landed.
        } else if now_playing && self.told_playing {
            // Playing: a playhead the GUI didn't set by its once-a-second
            // seek or a key's pre-step seek moves the player.
            let moved = self.seen_playhead != Some(playhead);
            let now = inp.now_ms;
            self.key_seeks
                .retain(|(at, _)| now.saturating_sub(*at) < SEEK_EVERY_MS);
            let own = self.auto_seek.is_some_and(|(_, f)| f == playhead)
                || self.key_seeks.iter().any(|(_, f)| *f == playhead);
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
            // drives it itself (D-40, step 9).
            if !self.scrub_holds(inp, playhead) {
                out.player.push(PlayerCmd::Seek(inp.ns(playhead)));
                self.shown_frame = Some(playhead);
            }
        }
        if !inp.scrubbing && self.scrubbed == Some(playhead) {
            // The snapshots caught up with the scrub.
            self.scrubbed = None;
            self.scrub_ended_at = None;
        }
        if fresh || !self.settling {
            self.seen_playhead = Some(playhead);
        }
        out
    }

    /// The program stopped playing (step 2): pause, the playhead parked at
    /// the player's frame, the rate back to 1.
    fn stop(&mut self, inp: &Inputs<'_>, out: &mut Out) {
        if self.told_playing {
            out.player.push(PlayerCmd::Pause);
            self.told_playing = false;
            out.playing = Some(false);
        }
        if self.ended {
            // `Seek{End}` (or `Start`) went out with the `Pause`.
            self.shown_frame = None;
        } else if self.settling {
            // The program's load after a take hasn't landed: the player's
            // position is still the take's. The playhead stays, and the
            // paused player goes back to it.
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
                self.scrubbed = None;
                self.scrub_ended_at = None;
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
                self.settling = true;
                self.reload_due = None;
                self.scrubbed = None;
                self.scrub_ended_at = None;
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
        self.check_settled(inp);
        let mut out = self.event_step(event, inp);
        self.report(inp, &mut out);
        out
    }

    fn event_step(&mut self, event: &PlayerEvent, inp: &Inputs<'_>) -> Out {
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
                if matches!(self.content, Content::Take(_)) {
                    // A bin preview stays paused (step 6): its `playing`
                    // would play it again at once, into the same failure,
                    // a toast each round (R17). The next play starts it
                    // from 0, not at the failing spot.
                    self.take_ended = true;
                    out.take_preview_ended = true;
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
    /// One timeout to the machine's next deadline (a slot: empty again
    /// after a caught panic, R65).
    timer: guard::Slot,
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
            timer: guard::Slot::new(),
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
                    // Not while the program plays: the router told the
                    // player itself (D-44). A take playing doesn't count.
                    if !own && !local.program_playing {
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
        // The playing program's playhead goes out before the host closes.
        let weak = Rc::downgrade(&sync);
        ctx.store
            .subscribe_close(move |_| {
                if let Some(sync) = weak.upgrade() {
                    sync.close();
                }
            })
            .detach();
        SYNC.with(|s| *s.borrow_mut() = Some(Rc::clone(&sync)));
        sync
    }

    /// The window closes its host next ([`SyncMachine::on_close`]).
    fn close(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let plan = self.plan(&state);
        let cmd = {
            let mut inp = self.inputs(&state, &plan);
            // The window says it is closing already (not open): its player
            // still plays until the media stop right after.
            inp.has_player = self.ctx.media.player().is_some();
            self.machine.borrow_mut().on_close(&inp)
        };
        if let Some(cmd) = cmd {
            tracing::debug!("program sync: the playhead before the close");
            self.ctx.store.dispatch(cmd);
        }
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
            player_idle: player.as_ref().is_none_or(|p| p.idle()),
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

    /// A ruler drag's motion (D-40): see [`SyncMachine::scrub`].
    fn scrub(self: &Rc<Self>, frame: Frame) {
        let state = self.ctx.store.state();
        let plan = self.plan(&state);
        let out = {
            let inp = self.inputs(&state, &plan);
            self.machine.borrow_mut().scrub(frame, &inp)
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
                    PlayerCmd::Seek(ns) => {
                        tracing::debug!("program sync: seek {ns} ns");
                        player.seek(ns);
                    }
                    PlayerCmd::Scrub(ns) => {
                        tracing::debug!("program sync: scrub {ns} ns");
                        player.scrub(ns);
                    }
                    PlayerCmd::Play(rate) => {
                        tracing::debug!("program sync: play at {rate}");
                        player.play(rate);
                    }
                    PlayerCmd::Pause => {
                        tracing::debug!("program sync: pause");
                        player.pause();
                    }
                }
            }
        }
        for cmd in out.dispatch {
            self.ctx.store.dispatch(cmd);
        }
        if let Some(frame) = out.park {
            self.park_for_preview(frame);
        }
        if let Some(notice) = out.notice {
            self.ctx.store.notify(notice);
        }
        let playing = out.playing;
        let program_playing = out.program_playing;
        let rate = out.rate;
        if let Some(rate) = rate {
            self.own_rate.set(Some(rate));
        }
        if playing.is_some()
            || program_playing.is_some()
            || rate.is_some()
            || out.take_preview_ended
        {
            let ended = out.take_preview_ended;
            self.ctx.store.update_local(move |l| {
                if let Some(playing) = playing {
                    l.playing = playing;
                }
                if let Some(program) = program_playing {
                    l.program_playing = program;
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

    /// The bin preview took the player from the playing program (step 8):
    /// `Seek{To(frame)}` parks its playhead at the frame the player showed.
    /// Until its reply the preview takes a playhead change as the
    /// program's own, not a timeline action (`TakePreview::parking`, D-17):
    /// the park's, and a once-a-second seek still on its way.
    fn park_for_preview(&self, frame: Frame) {
        let set = |parking: bool| {
            move |l: &mut crate::store::LocalUi| {
                if let Some(preview) = &mut l.take_preview {
                    preview.parking = parking;
                }
            }
        };
        self.ctx.store.update_local(set(true));
        let store = self.ctx.store.downgrade();
        self.ctx.store.dispatch_then(seek_to(frame), move |_| {
            if let Some(store) = store.upgrade() {
                store.update_local(set(false));
            }
        });
    }

    /// One timeout to the machine's next deadline.
    fn arm_timer(self: &Rc<Self>) {
        let deadline = self.machine.borrow().deadline();
        if deadline == self.timer_at.get() && self.timer.is_running() {
            return;
        }
        self.timer.stop();
        self.timer_at.set(deadline);
        let Some(at) = deadline else { return };
        let wait = at.saturating_sub(self.now_ms()).max(1);
        let weak = Rc::downgrade(self);
        self.timer.timeout_once(
            Duration::from_millis(wait),
            "program sync: the timer",
            move || {
                if let Some(sync) = weak.upgrade() {
                    sync.timer_at.set(None);
                    sync.run();
                }
            },
        );
    }
}

/// The key router took the player's `frame` for the playhead (G§3.8: a
/// press while the program plays): its pre-step seek is the GUI's own
/// ([`SyncMachine::note_own_seek`]).
pub fn key_took_player_frame(frame: Frame) {
    let sync = SYNC.with(|s| s.borrow().clone());
    if let Some(sync) = sync {
        let now = sync.now_ms();
        sync.machine.borrow_mut().note_own_seek(now, frame);
    }
}

/// A ruler drag's motion in the timeline (D-40): the stage's sync owns
/// the player's commands, so the timeline scrubs through it (and dispatches
/// its `Seek`s itself).
pub fn scrub(frame: Frame) {
    let sync = SYNC.with(|s| s.borrow().clone());
    if let Some(sync) = sync {
        sync.scrub(frame);
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
        player_idle: bool,
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
                player_idle: true,
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
                player_idle: self.player_idle,
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

        fn scrub(&mut self, frame: Frame) -> Out {
            let mut machine = std::mem::take(&mut self.machine);
            let out = machine.scrub(frame, &self.inputs());
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
        // A ruler drag (D-40): it scrubs the player through the sync, and
        // the snapshots of its throttled seeks (behind the scrubs) don't
        // move it.
        rig.scrubbing = true;
        rig.own_seek = Some(3000);
        assert_eq!(rig.scrub(3050).player, [PlayerCmd::Scrub(rig.ns(3050))]);
        assert_eq!(rig.scrub(3100).player, [PlayerCmd::Scrub(rig.ns(3100))]);
        rig.position = 3100;
        rig.own_seek = Some(3050);
        rig.state.ui.playhead = 3050;
        assert_eq!(rig.sync().player, []);
        // The release: the drag ends before its last seek's snapshot comes
        // (the playhead still 3050). The player is on 3100 already: no
        // seek back to 3050, and none when the snapshot reaches 3100.
        rig.scrubbing = false;
        rig.own_seek = Some(3100);
        rig.now += 20;
        assert_eq!(rig.sync().player, []);
        rig.state.ui.playhead = 3100;
        rig.now += 20;
        assert_eq!(rig.sync().player, []);
        // Then the playhead leads again (a key step, the CLI).
        rig.state.ui.playhead = 3200;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(3200))]);
    }

    /// A scrub whose last seek never echoes (refused) holds the player for
    /// at most SCRUB_ECHO_MS: the sync runs again then (its deadline) and
    /// follows the playhead.
    #[test]
    fn a_scrub_whose_seek_never_echoes_lets_go_after_a_second() {
        let mut rig = Rig::new(program()).loaded();
        rig.scrubbing = true;
        assert_eq!(rig.scrub(3100).player, [PlayerCmd::Scrub(rig.ns(3100))]);
        rig.scrubbing = false;
        rig.own_seek = Some(3100);
        assert_eq!(rig.sync().player, []);
        assert_eq!(rig.machine.deadline(), Some(rig.now + SCRUB_ECHO_MS));
        rig.now += SCRUB_ECHO_MS;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(2437))]);
        assert_eq!(rig.machine.deadline(), None);
    }

    /// While playing, in Live, or with a take in the player, a ruler drag
    /// leaves the player to its seeks (a scrub would stop playback or seek
    /// the wrong content).
    #[test]
    fn a_scrub_only_drives_a_paused_program_player() {
        let mut rig = Rig::new(program()).loaded();
        rig.scrubbing = true;
        rig.play();
        assert_eq!(rig.scrub(3000), Out::default());
        rig.pause();
        rig.trim = Some((TakeId::numbered(3), 10));
        rig.sync();
        assert_eq!(rig.scrub(3000), Out::default());
        rig.trim = None;
        rig.state.ui.canvas_mode = CanvasMode::Live;
        rig.sync();
        assert_eq!(rig.scrub(3000), Out::default());
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

    /// R00, R16: a bin preview plays a take, not the program. The player
    /// plays (`LocalUi.playing`), but the program-playing fact the timeline,
    /// the transport and the key router follow stays false, so they keep
    /// the program's playhead; a preview that takes over a playing program
    /// pauses it (the core doesn't play under the preview).
    #[test]
    fn a_bin_preview_is_never_the_program_playing() {
        let mut rig = Rig::new(program()).loaded();
        let take4 = TakeId::numbered(4);
        // Paused: the take plays, the program doesn't.
        rig.take_preview = Some((take4.clone(), true));
        let out = rig.sync();
        assert_eq!(out.playing, Some(true), "the player plays the take");
        assert_eq!(out.program_playing, None, "the program doesn't: {out:?}");
        assert_eq!(out.dispatch, []);
        rig.take_preview = None;
        assert_eq!(rig.sync().program_playing, None);
        // Playing: the program plays; a preview pauses it.
        let out = rig.play();
        assert_eq!(out.program_playing, Some(true));
        rig.position = 2460;
        rig.take_preview = Some((take4.clone(), true));
        let out = rig.sync();
        assert_eq!(out.program_playing, Some(false), "{out:?}");
        assert_eq!(out.dispatch, [Command::Pause], "the program pauses");
        assert_eq!(out.park, Some(2460), "parked where it showed");
        // The pause's snapshot: nothing more (no second pause or park).
        rig.state.runtime.playing = false;
        let out = rig.sync();
        assert_eq!(out.dispatch, []);
        assert_eq!(out.park, None);
        assert_eq!(out.program_playing, None);
        assert_eq!(out.player, []);
    }

    /// R16's follow-up: a bin preview that takes over a playing program
    /// parks its playhead at the frame the player showed (`Pause`, then the
    /// park's `Seek{To(f)}`, as a stop does, step 2), not at the last
    /// once-a-second seek, up to a second behind; the program comes back
    /// there when the preview ends. Nothing to park while the program
    /// doesn't play.
    #[test]
    fn a_preview_parks_a_playing_programs_playhead() {
        let mut rig = Rig::new(program()).loaded();
        let take4 = TakeId::numbered(4);
        assert_eq!(rig.play().player, [PlayerCmd::Play(1.0)]);
        rig.now += 600;
        rig.position = 2455;
        assert_eq!(rig.sync(), Out::default(), "before the once-a-second seek");
        rig.take_preview = Some((take4.clone(), true));
        let out = rig.sync();
        assert_eq!(out.dispatch, [Command::Pause], "{out:?}");
        assert_eq!(out.park, Some(2455), "parked at the player's frame");
        // The take loads after the position was read.
        assert!(
            matches!(&out.player[..], [PlayerCmd::Load { at_ns: 0, .. }, ..]),
            "{out:?}"
        );
        // The pause's and the park's snapshots: nothing more.
        rig.position = 30;
        rig.state.runtime.playing = false;
        rig.state.ui.playhead = 2455;
        let out = rig.sync();
        assert_eq!((&out.dispatch[..], out.park), (&[][..], None), "{out:?}");
        // The preview ends: the program comes back where it was parked.
        rig.take_preview = None;
        let out = rig.sync();
        assert!(
            matches!(&out.player[..], [PlayerCmd::Load { at_ns, .. }, ..] if *at_ns == rig.ns(2455)),
            "{out:?}"
        );
        // A preview over the paused program parks nothing.
        rig.player_idle = true;
        rig.position = 2455;
        rig.sync();
        rig.take_preview = Some((take4, true));
        let out = rig.sync();
        assert_eq!((&out.dispatch[..], out.park), (&[][..], None), "{out:?}");
    }

    /// R00: after a preview the program counts as playing only once its
    /// load landed: until the player is idle its position is the take's,
    /// which must reach neither the readers nor the once-a-second seek.
    #[test]
    fn after_a_preview_the_program_plays_once_its_load_landed() {
        let mut rig = Rig::new(program()).loaded();
        rig.take_preview = Some((TakeId::numbered(4), true));
        rig.sync();
        rig.position = 45;
        // Space with the timeline focused: `Play` ends the preview.
        rig.take_preview = None;
        rig.state.runtime.playing = true;
        let out = rig.sync();
        assert!(
            matches!(
                &out.player[..],
                [PlayerCmd::Load { at_ns, .. }, PlayerCmd::Play(_)] if *at_ns == rig.ns(2437)
            ),
            "{out:?}"
        );
        assert_eq!(out.program_playing, None, "not before the load landed");
        // The load is on its way: no own seek to the take's frame 45.
        rig.player_idle = false;
        rig.now += SEEK_EVERY_MS;
        let out = rig.sync();
        assert_eq!(out.dispatch, [], "no seek to the take's frame");
        assert_eq!(out.program_playing, None);
        let poll = rig.machine.deadline().expect("a poll");
        assert!(poll <= rig.now + SETTLE_POLL_MS, "{poll}");
        // It landed: the program plays, from its playhead.
        rig.player_idle = true;
        rig.position = 2450;
        let out = rig.sync();
        assert_eq!(out.program_playing, Some(true), "{out:?}");
        assert_eq!(
            out.dispatch,
            [seek_cmd(2450)],
            "the own seek, the program's"
        );
    }

    /// R00: stopped before the program's load after a preview landed, the
    /// playhead stays (the position is still the take's) and the paused
    /// player goes back to it.
    #[test]
    fn a_stop_before_the_programs_load_landed_keeps_the_playhead() {
        let mut rig = Rig::new(program()).loaded();
        rig.take_preview = Some((TakeId::numbered(4), true));
        rig.sync();
        rig.position = 45;
        rig.take_preview = None;
        rig.state.runtime.playing = true;
        rig.sync();
        rig.player_idle = false;
        let out = rig.pause();
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(out.dispatch, [], "no seek to the take's frame");
        rig.player_idle = true;
        rig.position = 2437;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(2437))]);
    }

    /// R17: a bin preview whose take fails stops there (G§3.10 step 6:
    /// "stays paused"), instead of `Play` → `Error` → `Play` with a toast
    /// each round; the person's next `Space` tries again, from the start.
    #[test]
    fn a_failing_preview_stops_until_played_again() {
        let mut rig = Rig::new(program()).loaded();
        let take4 = TakeId::numbered(4);
        rig.take_preview = Some((take4.clone(), true));
        rig.sync();
        let out = rig.event(PlayerEvent::Error {
            message: "Internal data stream error.".into(),
        });
        assert!(out.notice.is_some(), "a toast");
        assert!(out.take_preview_ended, "{out:?}");
        assert_eq!(out.dispatch, [], "the program's playhead stays");
        // The window's `apply`: the preview is paused now.
        rig.take_preview = Some((take4.clone(), !out.take_preview_ended));
        assert_eq!(rig.sync().player, [], "no Play after the error");
        assert_eq!(rig.sync().player, []);
        // Space: once, from the start.
        rig.take_preview = Some((take4, true));
        assert_eq!(
            rig.sync().player,
            [PlayerCmd::Seek(0), PlayerCmd::Play(1.0)]
        );
    }

    /// R21: Tab to Live (or a record start from Program, D-05) while
    /// playing stops playback as a pause does (step 2): the playhead parks
    /// at the player's frame, not at the last once-a-second seek, and the
    /// rate goes back to 1.
    #[test]
    fn leaving_program_while_playing_parks_the_playhead() {
        let mut rig = Rig::new(program()).loaded();
        rig.rate = 4.0;
        rig.machine.arm_rate(rig.now);
        assert_eq!(rig.play().player, [PlayerCmd::Play(4.0)]);
        rig.position = 2457;
        rig.now += 600;
        assert_eq!(rig.sync(), Out::default(), "before the once-a-second seek");
        // `SetCanvasMode{Live}`'s snapshot: Live, and not playing.
        rig.state.ui.canvas_mode = CanvasMode::Live;
        rig.state.runtime.playing = false;
        let out = rig.sync();
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(
            out.dispatch,
            [seek_cmd(2457)],
            "parked at the player's frame"
        );
        assert_eq!(out.playing, Some(false));
        assert_eq!(out.rate, Some(1.0));
        // The seek's snapshot, then back to Program: the player stays.
        rig.rate = 1.0;
        rig.state.ui.playhead = 2457;
        assert_eq!(rig.sync(), Out::default());
        rig.state.ui.canvas_mode = CanvasMode::Program;
        assert_eq!(rig.sync().player, [], "no seek back to 2437");
    }

    /// R21's guard: a bin preview's take that played on into Live stops
    /// without its frame reaching the program's playhead.
    #[test]
    fn a_preview_stopped_by_live_never_moves_the_playhead() {
        let mut rig = Rig::new(program()).loaded();
        rig.take_preview = Some((TakeId::numbered(4), true));
        rig.sync();
        rig.position = 45;
        rig.take_preview = None;
        rig.state.ui.canvas_mode = CanvasMode::Live;
        let out = rig.sync();
        assert_eq!(out.player, [PlayerCmd::Pause]);
        assert_eq!(out.dispatch, [], "no take frame in the playhead");
    }

    /// R22: the key router's pre-step `Seek{To(player frame)}` is the
    /// GUI's own: at 4× (or stepping in reverse) the player is past that
    /// frame by the time its snapshot comes, and seeking it back made
    /// playback stutter. An auto seek still unechoed when a key comes
    /// stays own too.
    #[test]
    fn a_keys_seek_to_the_players_frame_never_pulls_it_back() {
        let mut rig = Rig::new(program()).loaded();
        rig.rate = 4.0;
        rig.machine.arm_rate(rig.now);
        assert_eq!(rig.play().player, [PlayerCmd::Play(4.0)]);
        rig.now += 300;
        rig.position = 2500;
        // `s` while playing: the router reads the player's frame and
        // seeks the playhead there first.
        rig.machine.note_own_seek(rig.now, 2500);
        rig.own_seek = Some(2500);
        rig.now += 30;
        rig.position = 2504;
        rig.state.ui.playhead = 2500;
        assert_eq!(rig.sync().player, [], "no pull back to 2500");
        // Reverse at −2: the player steps 4 frames back meanwhile.
        rig.now += 1000;
        rig.position = 2600;
        let auto = rig.sync();
        assert_eq!(auto.dispatch, [seek_cmd(2600)], "an auto seek");
        rig.machine.note_own_seek(rig.now, 2596);
        rig.position = 2592;
        // The auto seek's snapshot, then the key's: neither moves it.
        rig.state.ui.playhead = 2600;
        assert_eq!(rig.sync().player, []);
        rig.state.ui.playhead = 2596;
        assert_eq!(rig.sync().player, []);
        // A seek the GUI didn't take from the player (the ruler, the CLI)
        // still moves it.
        rig.state.ui.playhead = 100;
        assert_eq!(rig.sync().player, [PlayerCmd::Seek(rig.ns(100))]);
    }

    /// G§3.6 (A1's audit): at the close a playing program parks the
    /// playhead at the player's frame (the once-a-second seek still to
    /// come went out after the close, refused); nothing paused, nothing
    /// for a take in the player.
    #[test]
    fn the_close_parks_a_playing_programs_playhead() {
        let mut rig = Rig::new(program()).loaded();
        let mut machine = std::mem::take(&mut rig.machine);
        assert_eq!(machine.on_close(&rig.inputs()), None, "paused");
        rig.machine = machine;
        assert_eq!(rig.play().player, [PlayerCmd::Play(1.0)]);
        rig.now += 600;
        rig.position = rig.state.ui.playhead + 18;
        let mut machine = std::mem::take(&mut rig.machine);
        assert_eq!(
            machine.on_close(&rig.inputs()),
            Some(seek_cmd(rig.position)),
            "playing: the player's frame"
        );
        rig.machine = machine;
        // A bin preview's take in the player: the program isn't playing.
        let mut rig = Rig::new(program()).loaded();
        rig.take_preview = Some((TakeId::numbered(4), true));
        rig.sync();
        let mut machine = std::mem::take(&mut rig.machine);
        assert_eq!(machine.on_close(&rig.inputs()), None, "a take");
        rig.machine = machine;
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
