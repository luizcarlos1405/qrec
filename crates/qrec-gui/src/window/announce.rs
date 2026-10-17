//! Screen-reader announcements (WP10; PRODUCT Accessibility: the start and
//! the end of a take and the end of an export are announced), through
//! `gtk::Accessible::announce` on the window: polite for news, assertive
//! for failures.
//!
//! - The record button announces what no toast says (`header/record.rs`):
//!   a take starting to record, in the ghost's words ("Recording take 10",
//!   "Recording take 10 over c2d0 (take 5)"), and a take landing on the
//!   timeline, in its stop's reply's ("Recorded take 10 (2 min 41.4 s)",
//!   `on_event::record::recorded_message`).
//! - The toast host announces each toast as it comes (`toasts.rs`), with
//!   its title and the first line of a body that isn't a path: a take's
//!   other ends (too short, cancelled, broken), the export's end (done or
//!   failed, also when the export panel shows it and drops the toast), and
//!   the rest, as libadwaita's toast overlay does. A toast that is the
//!   newest one again (a held key's repeats) isn't announced again.
//!
//! GTK's test context (`GTK_A11Y=test`, or the one GTK falls back to when it
//! finds no accessibility bus) has no `announce` in GTK 4.22: the call
//! would jump to a null function pointer. So the window only calls GTK with
//! another context; every announcement is logged (`announce (polite): …`)
//! either way and, in test mode, kept for the test channel (`local`'s
//! `announcements`, G§3.18).

use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::prelude::*;
use qrec_core::on_event::record::recorded_message;
use qrec_core::protocol::ToastLevel;
use qrec_core::state::{RecordPhase, State};
use qrec_core::time::{duration_label, frames_to_ns};
use qrec_core::view::notices::Notice;
use qrec_core::view::timeline::{Viewport, ghost};
use qrec_core::{TakeId, TakeStatus};
use serde_json::{Value, json};

/// How urgently a screen reader says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// At the next pause (AT-SPI's polite live region).
    Polite,
    /// At once (AT-SPI's assertive live region): failures only.
    Assertive,
}

impl Priority {
    pub fn name(self) -> &'static str {
        match self {
            Priority::Polite => "polite",
            Priority::Assertive => "assertive",
        }
    }

    /// GTK's: `Medium` is AT-SPI's polite, `High` its assertive.
    fn gtk(self) -> gtk::AccessibleAnnouncementPriority {
        match self {
            Priority::Polite => gtk::AccessibleAnnouncementPriority::Medium,
            Priority::Assertive => gtk::AccessibleAnnouncementPriority::High,
        }
    }
}

/// One announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub message: String,
    pub priority: Priority,
}

impl Announcement {
    pub fn polite(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            priority: Priority::Polite,
        }
    }

    pub fn assertive(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            priority: Priority::Assertive,
        }
    }
}

/// What a snapshot change says about the record loop (`prev` → `next`):
/// a take that starts recording, or one that lands on the timeline. The
/// take's other ends come with the core's toasts (too short, cancelled,
/// broken), announced as toasts.
pub fn record_change(prev: &State, next: &State) -> Option<Announcement> {
    match (&prev.runtime.record, &next.runtime.record) {
        (RecordPhase::Recording { take: was, .. }, RecordPhase::Recording { take, .. })
            if was == take =>
        {
            None
        }
        (_, RecordPhase::Recording { take, .. }) => Some(started(next, take)),
        (before, RecordPhase::Idle) => landed(next, before.take()?),
        _ => None,
    }
}

/// "Recording take 10", and the clip a record-over replaces in the ghost's
/// words ("over c2d0 (take 5)", G§4.5).
fn started(state: &State, take: &TakeId) -> Announcement {
    let n = take.number();
    let over = ghost(state, &Viewport::default(), 0).and_then(|g| g.over);
    Announcement::polite(match over {
        Some(over) => format!("Recording take {n} {over}"),
        None => format!("Recording take {n}"),
    })
}

/// A take that left the record loop: placed on the timeline, it says what
/// its stop's reply says; `None` otherwise (its toast says why).
fn landed(state: &State, take: &TakeId) -> Option<Announcement> {
    let t = state.project.takes.get(take)?;
    if t.status != TakeStatus::Ok || !t.placed {
        return None;
    }
    let fps = state.project.canvas.fps;
    let length = duration_label(frames_to_ns(t.duration_frames, fps));
    let replaced: Vec<_> = t.replaced.iter().map(|c| c.id.clone()).collect();
    Some(Announcement::polite(recorded_message(
        take.number(),
        &length,
        &replaced,
    )))
}

/// What a toast says: its title, then the first line of its body unless
/// the body is a path (`exports/….mp4 · 88.4 MB`); assertive for an error.
/// The CLI-style backticks of the core's copy (`` `qrec history` ``, a
/// binding's id) aren't said: a screen reader reads them out as they are
/// (WP10's code audit).
pub fn toast(notice: &Notice) -> Announcement {
    let mut message = notice.title.trim().to_owned();
    let line = notice
        .body
        .as_deref()
        .filter(|_| !crate::toasts::body_is_path(notice))
        .and_then(|body| body.lines().map(str::trim).find(|l| !l.is_empty()));
    if let Some(line) = line {
        let ended = message.ends_with(['.', '!', '?', '\u{2026}', ':']);
        message.push_str(if ended || message.is_empty() {
            " "
        } else {
            ". "
        });
        message.push_str(line);
    }
    let message = message.replace('`', "");
    match notice.level {
        ToastLevel::Error => Announcement::assertive(message),
        _ => Announcement::polite(message),
    }
}

thread_local! {
    /// What was announced, in test mode (the test channel's `local`).
    static ANNOUNCED: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

/// The announcements so far (test mode): `{message, priority, sent}`,
/// `sent` false when GTK had no context that announces.
pub fn announced() -> Vec<Value> {
    ANNOUNCED.with(|a| a.borrow().clone())
}

/// Announces `what` on `widget`'s window (or `widget` itself before it is
/// in one).
pub fn announce(widget: &impl IsA<gtk::Widget>, what: &Announcement) {
    let widget = widget.as_ref();
    let target: gtk::Widget = match widget.root() {
        Some(root) => root.upcast(),
        None => widget.clone(),
    };
    let sent = announces(&target);
    tracing::info!(
        "announce ({}{}): {}",
        what.priority.name(),
        if sent {
            ""
        } else {
            ", no screen reader context"
        },
        what.message
    );
    if crate::testctl::mode().on {
        ANNOUNCED.with(|a| {
            a.borrow_mut().push(json!({
                "message": what.message,
                "priority": what.priority.name(),
                "sent": sent,
            }));
        });
    }
    if sent {
        target.announce(&what.message, what.priority.gtk());
    }
}

/// Whether GTK can announce on `widget`: it has an accessibility context
/// (none under `GTK_A11Y=none`), and not GTK's test context, whose
/// `announce` is missing in GTK 4.22.
fn announces(widget: &gtk::Widget) -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    let disabled = *DISABLED.get_or_init(|| {
        std::env::var("GTK_A11Y").is_ok_and(|v| v.trim().eq_ignore_ascii_case("none"))
    });
    // `at_context` can't be asked without one (GTK returns NULL).
    !disabled && widget.at_context().type_().name() != TEST_CONTEXT
}

/// GTK's test accessibility context (`GtkTestATContext`).
const TEST_CONTEXT: &str = "GtkTestATContext";

#[cfg(test)]
mod tests {
    use qrec_core::ClipId;
    use qrec_core::state::{ClipRef, RecordMode};
    use qrec_core::testkit;

    use super::*;

    fn recording(state: &State, take: u32, mode: RecordMode) -> State {
        let mut next = state.clone();
        next.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(take),
            mode,
            elapsed_ns: 0,
            waiters: Vec::new(),
        };
        next
    }

    fn finalizing(state: &State, take: u32) -> State {
        let mut next = state.clone();
        next.runtime.record = RecordPhase::Finalizing {
            take: TakeId::numbered(take),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: Vec::new(),
            waiters: Vec::new(),
            discard: None,
        };
        next
    }

    #[test]
    fn a_take_that_starts_recording_is_announced_once() {
        let idle = testkit::design_state();
        let rec = recording(&idle, 10, RecordMode::Append);
        assert_eq!(
            record_change(&idle, &rec),
            Some(Announcement::polite("Recording take 10"))
        );
        // The clock ticking isn't news.
        let mut later = rec.clone();
        if let RecordPhase::Recording { elapsed_ns, .. } = &mut later.runtime.record {
            *elapsed_ns = 2_000_000_000;
        }
        assert_eq!(record_change(&rec, &later), None);
    }

    #[test]
    fn a_record_over_names_the_clip_it_replaces() {
        let idle = testkit::design_state();
        let c2d0 = idle
            .project
            .clip(&ClipId::from_raw("c2d0"))
            .cloned()
            .expect("c2d0");
        let over = RecordMode::Over {
            clips: vec![ClipRef {
                id: c2d0.id.clone(),
                take: c2d0.take.clone(),
                in_: c2d0.in_,
                out: c2d0.out,
            }],
        };
        let rec = recording(&idle, 10, over);
        assert_eq!(
            record_change(&idle, &rec),
            Some(Announcement::polite("Recording take 10 over c2d0 (take 5)"))
        );
    }

    /// The design project's take 8 (3,105 frames, placed) and take 5 (2,214
    /// frames, placed over c4a7) as if they had just landed.
    #[test]
    fn a_take_that_lands_says_what_its_reply_says() {
        let landed = testkit::design_state();
        assert_eq!(
            record_change(&finalizing(&landed, 8), &landed),
            Some(Announcement::polite("Recorded take 8 (1 min 43.5 s)"))
        );
        assert_eq!(
            record_change(&finalizing(&landed, 5), &landed),
            Some(Announcement::polite(
                "Recorded take 5 (1 min 13.8 s) over c4a7"
            ))
        );
        // From recording straight to idle (the queue coalesced the
        // finalizing snapshot): the same.
        assert_eq!(
            record_change(&recording(&landed, 8, RecordMode::Append), &landed),
            Some(Announcement::polite("Recorded take 8 (1 min 43.5 s)"))
        );
        // Not placed: too short (take 6), broken (take 9), cancelled (take
        // 8 left in the bin); its toast says so.
        for n in [6, 9] {
            assert_eq!(
                record_change(&finalizing(&landed, n), &landed),
                None,
                "take {n}"
            );
        }
        let mut cancelled = landed.clone();
        if let Some(t) = cancelled.project.takes.get_mut(&TakeId::numbered(8)) {
            t.placed = false;
        }
        assert_eq!(record_change(&finalizing(&landed, 8), &cancelled), None);
        // A countdown that ends without a take, and idle snapshots: nothing.
        let mut countdown = landed.clone();
        countdown.runtime.record = RecordPhase::Countdown {
            remaining: 2,
            mode: RecordMode::Append,
            timer: qrec_core::TimerId(1),
            start_req: None,
            wait: false,
        };
        assert_eq!(record_change(&countdown, &landed), None);
        assert_eq!(record_change(&landed, &landed), None);
    }

    #[test]
    fn toasts_say_their_title_and_a_line_of_their_body() {
        use qrec_core::on_event::output::{EXPORT_DONE_TITLE, EXPORT_FAILED_TITLE};
        use qrec_core::protocol::{ToastAction, UiAction};
        use qrec_core::view::notices::NoticeAction;
        let path = "exports/rust-traits-talk-20261014-153012.mp4";
        let done = Notice::new(ToastLevel::Success, EXPORT_DONE_TITLE)
            .with_body(format!("{path} \u{b7} 88.4 MB"))
            .with_action(NoticeAction::Core(ToastAction {
                label: "Open file".into(),
                action: UiAction::OpenPath(path.into()),
            }));
        assert_eq!(toast(&done), Announcement::polite(EXPORT_DONE_TITLE));
        let failed = Notice::new(ToastLevel::Error, EXPORT_FAILED_TITLE)
            .with_body("ffmpeg exited with status 1\n\nError opening output");
        assert_eq!(
            toast(&failed),
            Announcement::assertive("Export failed. ffmpeg exited with status 1")
        );
        let cancelled = Notice::new(ToastLevel::Info, "Take 10 cancelled")
            .with_body("It is in the bin; nothing was added to the timeline.");
        assert_eq!(
            toast(&cancelled),
            Announcement::polite(
                "Take 10 cancelled. It is in the bin; nothing was added to the timeline."
            )
        );
        let short = Notice::new(
            ToastLevel::Warning,
            "That was shorter than Lord Farquaad. Take moved to the bin.",
        );
        assert_eq!(
            toast(&short),
            Announcement::polite("That was shorter than Lord Farquaad. Take moved to the bin.")
        );
        // The copy's CLI-style backticks aren't said.
        let unknown = Notice::new(ToastLevel::Warning, "keys.toml has a problem")
            .with_body("keys.toml: unknown binding `nope.nothing` (`qrec keys` lists the ids)");
        assert_eq!(
            toast(&unknown),
            Announcement::polite(
                "keys.toml has a problem. keys.toml: unknown binding nope.nothing (qrec keys \
                 lists the ids)"
            )
        );
    }
}
