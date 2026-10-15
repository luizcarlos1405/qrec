//! The Program player (GUI H-2, docs/GUI.md G§3.10): one GES timeline and
//! pipeline per session on thread `qrec-player`, rebuilt in place by
//! [`ProgramPlayer::load`] and played into the GUI's Program
//! `gtk4paintablesink`. Accurate seeks; forward rates are seeks with a
//! rate, reverse is stepping; the GTK thread reads the position from an
//! atomic. Every call returns at once. Owner: capture.
//!
//! Not built yet: [`ProgramPlayer::spawn`] fails with `Unsupported`, so the
//! GUI has no Program picture.

use std::path::PathBuf;

use qrec_core::plans::GesTimeline;

/// How to build a player.
pub struct PlayerOptions {
    /// Plan paths are joined with it.
    pub project_dir: PathBuf,
    /// The Program `gtk4paintablesink`, made on the GTK thread.
    pub video_sink: gst::Element,
    pub audio: AudioOut,
    /// `None`: composite at the canvas size; `Some`: at this size, for a
    /// machine that can't play at canvas size (GUI H-9).
    pub out_size: Option<(u32, u32)>,
    /// Called on the player thread; must return at once.
    pub events: Box<dyn Fn(PlayerEvent) + Send + Sync>,
}

impl std::fmt::Debug for PlayerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerOptions")
            .field("project_dir", &self.project_dir)
            .field("audio", &self.audio)
            .field("out_size", &self.out_size)
            .finish_non_exhaustive()
    }
}

/// Where the player's sound goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOut {
    /// `pulsesink client-name=Qrec`; a `fakesink` and a warning when it
    /// can't start.
    System,
    /// `fakesink sync=true`: the fake backend and tests.
    Silent,
}

/// What the player reports (through [`PlayerOptions::events`]).
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// A plan is in place; its length.
    Loaded { duration_ns: u64 },
    /// Playback reached the end (the start, playing in reverse).
    Ended { reverse: bool },
    /// Playback stopped on an error; the player stays paused.
    Error { message: String },
    /// Frames dropped as late since the last report (QoS).
    Late { dropped: u64 },
    /// After [`ProgramPlayer::shutdown`]: the pipeline reached NULL.
    Stopped,
}

/// Plays the program timeline (or one take) for the Program picture.
#[derive(Debug)]
pub struct ProgramPlayer {
    _private: (),
}

impl ProgramPlayer {
    /// Starts the player thread `qrec-player`.
    pub fn spawn(opts: PlayerOptions) -> std::io::Result<Self> {
        let _ = opts;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Program playback isn't built yet",
        ))
    }

    /// Shows `plan` from `at_ns`, playing or paused as before (the newest
    /// load wins).
    pub fn load(&self, plan: GesTimeline, at_ns: u64) {
        let _ = (plan, at_ns);
    }

    /// An accurate seek; one at a time, the newest wins.
    pub fn seek(&self, ns: u64) {
        let _ = ns;
    }

    /// Plays at `rate` (1, 2, 4; negative steps backwards), also changing
    /// the rate while playing.
    pub fn play(&self, rate: f64) {
        let _ = rate;
    }

    pub fn pause(&self) {}

    /// A seek plus 60 ms of sound (ruler scrubbing).
    pub fn scrub(&self, ns: u64) {
        let _ = ns;
    }

    /// Where playback is, updated at least 60 times a second while playing.
    pub fn position_ns(&self) -> u64 {
        0
    }

    /// Stops the pipeline; [`PlayerEvent::Stopped`] once it reached NULL
    /// (within 5 s).
    pub fn shutdown(&self) {}
}
