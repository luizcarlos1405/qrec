//! No panic crosses into C (G§3.12). Every closure GLib or GTK calls (a
//! source, a signal handler, an async callback, a tick callback, a
//! subclass's virtual method) goes through this module: a panic in it is
//! caught at the boundary, logged as an error with what was running, and
//! told once as an error toast; the window carries on. Unwinding into
//! GLib's C frames would abort the process instead (`panic_cannot_unwind`
//! in `glib::source::trampoline`: the queue's drain aborted that way in a
//! WP0b part 1 run, on a `RefCell` borrowed twice in startup).
//!
//! - Sources: [`idle`] (from any thread), [`idle_local_once`],
//!   [`timeout_local`], [`timeout_local_once`], [`invoke`], [`tick`]. Only
//!   this file calls GLib's own (a test greps the crate).
//! - Signal handlers and async callbacks: [`guarded!`](crate::guarded),
//!   `button.connect_clicked(guarded!("what", move |_| …))` (a test greps
//!   every `.connect_…(`).
//! - Virtual methods and other entry points: [`run`] or [`catch`].
//!
//! After a panic the closure returns [`AfterPanic::after_panic`]: nothing,
//! `false`, `Propagation::Proceed`, `ControlFlow::Break` (a repeating source
//! stops rather than panic again on every turn).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;

/// What a guarded closure returns after a panic.
pub trait AfterPanic {
    fn after_panic() -> Self;
}

impl AfterPanic for () {
    fn after_panic() {}
}

impl AfterPanic for bool {
    fn after_panic() -> Self {
        false
    }
}

impl AfterPanic for glib::Propagation {
    fn after_panic() -> Self {
        glib::Propagation::Proceed
    }
}

impl AfterPanic for glib::ControlFlow {
    fn after_panic() -> Self {
        glib::ControlFlow::Break
    }
}

impl AfterPanic for i32 {
    fn after_panic() -> Self {
        0
    }
}

impl AfterPanic for f64 {
    fn after_panic() -> Self {
        0.0
    }
}

impl AfterPanic for (i32, i32, i32, i32) {
    fn after_panic() -> Self {
        (0, 0, -1, -1)
    }
}

impl<T> AfterPanic for Option<T> {
    fn after_panic() -> Self {
        None
    }
}

/// Wraps a signal handler or an async callback: a panic in `body` is
/// caught and reported (G§3.12) and the handler returns
/// [`AfterPanic::after_panic`], or `after_panic = expr`.
///
/// ```ignore
/// button.connect_clicked(guarded!("header: commands", move |_| keys.run_binding("palette.open")));
/// window.connect_close_request(guarded!("close", after_panic = glib::Propagation::Stop, move |_| …));
/// ```
#[macro_export]
macro_rules! guarded {
    ($what:expr, after_panic = $after:expr, move |$($arg:pat_param),*| $body:expr) => {
        move |$($arg),*| $crate::guard::catch($what, || $body).unwrap_or_else(|| $after)
    };
    ($what:expr, move |$($arg:pat_param),*| $body:expr) => {
        move |$($arg),*| $crate::guard::run($what, || $body)
    };
}

/// Runs `f`; a panic is caught and reported, and `R::after_panic()` comes
/// back instead.
pub fn run<R: AfterPanic>(what: &str, f: impl FnOnce() -> R) -> R {
    catch(what, f).unwrap_or_else(R::after_panic)
}

/// Runs `f`; `None` when it panicked (the panic is reported).
pub fn catch<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(payload) => {
            report(what, payload.as_ref());
            None
        }
    }
}

/// A panic's message.
pub fn message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic without a message".to_owned()
    }
}

/// Who tells the person about a caught panic: (what was running, the
/// panic's message).
type Reporter = Box<dyn Fn(&str, &str)>;

thread_local! {
    static PANICS: Cell<u64> = const { Cell::new(0) };
    static LAST_TOLD: Cell<Option<Instant>> = const { Cell::new(None) };
    static REPORTER: RefCell<Option<Reporter>> = const { RefCell::new(None) };
}

/// How many panics were caught on this thread (the test channel reports
/// it; the smoke run expects none).
pub fn panics() -> u64 {
    PANICS.with(Cell::get)
}

/// Who tells the person about a caught panic (the app: an error toast).
pub fn set_reporter(f: impl Fn(&str, &str) + 'static) {
    REPORTER.with(|r| *r.borrow_mut() = Some(Box::new(f)));
}

/// How often a caught panic toasts (the log has every one).
const TELL_EVERY: Duration = Duration::from_secs(5);

fn report(what: &str, payload: &(dyn Any + Send)) {
    PANICS.with(|p| p.set(p.get() + 1));
    let message = message(payload);
    tracing::error!("{what} panicked: {message} (caught; the window carries on)");
    let now = Instant::now();
    let tell = LAST_TOLD.with(|last| {
        let due = last
            .get()
            .is_none_or(|t| now.duration_since(t) >= TELL_EVERY);
        if due {
            last.set(Some(now));
        }
        due
    });
    if !tell {
        return;
    }
    // The reporter is taken while it runs: a panic in it (or a report from
    // it) can't come back here.
    let reporter = REPORTER.with(|r| r.borrow_mut().take());
    if let Some(reporter) = reporter {
        if catch_unwind(AssertUnwindSafe(|| reporter(what, &message))).is_err() {
            tracing::error!("telling about a panic panicked too");
        }
        REPORTER.with(|r| {
            let mut slot = r.borrow_mut();
            if slot.is_none() {
                *slot = Some(reporter);
            }
        });
    }
}

// ------------------------------------------------------------- sources

/// `glib::idle_add_full` on the default main context, from any thread.
pub fn idle(
    priority: glib::Priority,
    what: &'static str,
    mut f: impl FnMut() -> glib::ControlFlow + Send + 'static,
) -> glib::SourceId {
    glib::idle_add_full(priority, move || run(what, &mut f))
}

/// `glib::idle_add_local_once` (main thread).
pub fn idle_local_once(what: &'static str, f: impl FnOnce() + 'static) -> glib::SourceId {
    glib::idle_add_local_once(move || run(what, f))
}

/// `glib::timeout_add_local` (main thread); a panic stops it.
pub fn timeout_local(
    interval: Duration,
    what: &'static str,
    mut f: impl FnMut() -> glib::ControlFlow + 'static,
) -> glib::SourceId {
    glib::timeout_add_local(interval, move || run(what, &mut f))
}

/// `glib::timeout_add_local_once` (main thread).
pub fn timeout_local_once(
    interval: Duration,
    what: &'static str,
    f: impl FnOnce() + 'static,
) -> glib::SourceId {
    glib::timeout_add_local_once(interval, move || run(what, f))
}

/// `MainContext::default().invoke`: `f` on the main thread, from any thread.
pub fn invoke(what: &'static str, f: impl FnOnce() + Send + 'static) {
    glib::MainContext::default().invoke(move || run(what, f));
}

/// `widget.add_tick_callback`; a panic removes it.
pub fn tick<W: IsA<gtk::Widget>>(
    widget: &W,
    what: &'static str,
    f: impl Fn(&W, &gtk::gdk::FrameClock) -> glib::ControlFlow + 'static,
) -> gtk::TickCallbackId {
    widget.add_tick_callback(move |w, clock| run(what, || f(w, clock)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_panic_is_caught_counted_and_told_once() {
        let told = std::rc::Rc::new(RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&told);
        set_reporter(move |what, message| seen.borrow_mut().push(format!("{what}: {message}")));
        let before = panics();
        assert!(!run::<bool>("a test", || panic!("boom")));
        assert_eq!(
            run::<glib::ControlFlow>("a source", || panic!("again")),
            glib::ControlFlow::Break
        );
        assert_eq!(catch("fine", || 7), Some(7));
        assert_eq!(panics(), before + 2);
        // Told once within TELL_EVERY.
        assert_eq!(*told.borrow(), ["a test: boom"]);
        let positive = guarded!("a handler", move |x| x > 0);
        assert!(positive(1));
        let checked = guarded!("a handler", move |x| {
            assert!(x > 0, "not positive");
            true
        });
        assert!(checked(2));
        assert!(!checked(-1), "after a panic: false");
        fn call(f: &impl Fn(bool) -> glib::Propagation, closing: bool) -> glib::Propagation {
            f(closing)
        }
        let stop = guarded!(
            "close",
            after_panic = glib::Propagation::Stop,
            move |closing| {
                let closing: bool = closing;
                assert!(!closing, "closing");
                glib::Propagation::Proceed
            }
        );
        assert_eq!(call(&stop, false), glib::Propagation::Proceed);
        assert_eq!(call(&stop, true), glib::Propagation::Stop);
    }

    #[test]
    fn a_failing_reporter_is_contained() {
        LAST_TOLD.with(|l| l.set(None));
        set_reporter(|_, _| panic!("the reporter itself"));
        let before = panics();
        run::<()>("outer", || panic!("first"));
        assert_eq!(panics(), before + 1, "the reporter's panic isn't counted");
    }

    /// G§3.12: GLib's source functions are only called here, and every
    /// signal connection and async callback goes through `guarded!`.
    #[test]
    fn every_callback_into_rust_is_guarded() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let relative = path.strip_prefix(&src).unwrap().display().to_string();
                let text = std::fs::read_to_string(&path).unwrap();
                // Production code only: tests may call GLib directly.
                let code = text.split("#[cfg(test)]").next().unwrap_or_default();
                offenders.extend(unguarded(&relative, code));
            }
        }
        assert!(offenders.is_empty(), "unguarded callbacks: {offenders:#?}");
    }

    /// The unguarded callbacks in `code` (comments stripped).
    fn unguarded(file: &str, code: &str) -> Vec<String> {
        let code: String = code
            .lines()
            .map(|l| l.split("//").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let mut found = Vec::new();
        let line_of = |at: usize| code[..at].lines().count();
        if file != "guard.rs" {
            for raw in ["idle_add", "timeout_add", ".invoke(", "add_tick_callback("] {
                for (at, _) in code.match_indices(raw) {
                    found.push(format!("{file}:{}: {raw} outside guard.rs", line_of(at)));
                }
            }
        }
        for marker in [".connect_", ".launch(", ".open_containing_folder("] {
            for (at, _) in code.match_indices(marker) {
                let rest = &code[at + marker.len()..];
                // `.connect_name(`: skip the signal name.
                let args = if marker == ".connect_" {
                    let Some(open) = rest.find('(') else { continue };
                    if !rest[..open]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        continue;
                    }
                    &rest[open + 1..]
                } else {
                    rest
                };
                // The callback is the last argument: `guarded!(` must come
                // before the closure starts.
                let closure = args.find("move |").or_else(|| args.find('|'));
                let guard = args.find("guarded!(");
                let ok = match (guard, closure) {
                    (Some(g), Some(c)) => g < c,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if !ok {
                    found.push(format!(
                        "{file}:{}: {marker}… without guarded!",
                        line_of(at)
                    ));
                }
            }
        }
        found
    }

    #[test]
    fn the_grep_sees_unguarded_callbacks() {
        let bad = "button.connect_clicked(move |_| go());\nglib::idle_add_local_once(|| {});";
        assert_eq!(unguarded("x.rs", bad).len(), 2);
        let good = "button.connect_clicked(guarded!(\"go\", move |_| go()));\n\
                    launcher.launch(None, NONE, guarded!(\"open\", move |r| f(r)));";
        assert!(
            unguarded("x.rs", good).is_empty(),
            "{:?}",
            unguarded("x.rs", good)
        );
        assert!(unguarded("guard.rs", "glib::idle_add_full(p, f)").is_empty());
    }
}
