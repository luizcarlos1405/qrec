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
//!   this file calls GLib's own (a test greps the crate). A [`Slot`] holds
//!   one its owner stops or starts again: it empties itself however the
//!   callback ends (an id kept past a caught panic is dead: a check of it
//!   waits forever, and `SourceId::remove` panics on it).
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
    report_message(what, &message(payload));
}

/// Tells about a panic caught elsewhere (an io job's, `media::io`), on the
/// main thread: logged, counted and toasted like the ones caught here.
pub fn report_message(what: &str, message: &str) {
    PANICS.with(|p| p.set(p.get() + 1));
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
        if catch_unwind(AssertUnwindSafe(|| reporter(what, message))).is_err() {
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

// ------------------------------------------------------------- slots

/// Where an owner keeps a repeating source, a one-shot timer or a tick
/// callback it starts through it ([`Slot::tick`], [`Slot::timeout`],
/// [`Slot::timeout_once`]; each guarded like [`tick`] and the others):
/// whatever ends the callback (it returned `Break` or fired, it panicked,
/// it was removed, its widget went) empties the slot, so the owner sees
/// whether one runs and starts it again when it needs one (a panic doesn't
/// leave it holding a dead id, waiting forever), and [`Slot::stop`] never
/// removes a source GLib already destroyed (`SourceId::remove` panics on
/// one). Main thread only.
#[derive(Default)]
pub struct Slot {
    inner: std::rc::Rc<SlotInner>,
}

#[derive(Default)]
struct SlotInner {
    /// The running callback's number; 0: none.
    running: Cell<u64>,
    /// The last number handed out.
    started: Cell<u64>,
    /// What stops the running one.
    id: Cell<Option<SlotId>>,
}

enum SlotId {
    Source(glib::SourceId),
    Tick(gtk::TickCallbackId),
}

/// Moved into a slot's callback: dropped with it (GLib's or GTK's destroy
/// notify, at the callback's end whatever ends it), it empties the slot
/// unless the slot started another one meanwhile. It touches `Cell`s only:
/// a destroy notify can run inside [`Slot::stop`].
struct Ended {
    slot: std::rc::Weak<SlotInner>,
    number: u64,
}

impl Ended {
    /// Keeps it in a closure that has nothing else to do with it.
    fn keep(&self) {}
}

impl Drop for Ended {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.upgrade()
            && slot.running.get() == self.number
        {
            slot.running.set(0);
            drop(slot.id.take());
        }
    }
}

impl Slot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether its callback runs (is still to fire, for a one-shot).
    pub fn is_running(&self) -> bool {
        self.inner.running.get() != 0
    }

    /// Stops the running one, if any (never one already gone).
    pub fn stop(&self) {
        self.inner.running.set(0);
        match self.inner.id.take() {
            Some(SlotId::Source(id)) => id.remove(),
            Some(SlotId::Tick(id)) => id.remove(),
            None => {}
        }
    }

    /// The sentinel of a new callback, which is now the running one.
    fn start(&self) -> Ended {
        self.stop();
        let number = self.inner.started.get() + 1;
        self.inner.started.set(number);
        self.inner.running.set(number);
        Ended {
            slot: std::rc::Rc::downgrade(&self.inner),
            number,
        }
    }

    /// [`tick`] in place of the running one.
    pub fn tick<W: IsA<gtk::Widget>>(
        &self,
        widget: &W,
        what: &'static str,
        f: impl Fn(&W, &gtk::gdk::FrameClock) -> glib::ControlFlow + 'static,
    ) {
        let ended = self.start();
        let id = widget.add_tick_callback(move |w, clock| {
            ended.keep();
            run(what, || f(w, clock))
        });
        self.inner.id.set(Some(SlotId::Tick(id)));
    }

    /// [`timeout_local`] in place of the running one.
    pub fn timeout(
        &self,
        interval: Duration,
        what: &'static str,
        mut f: impl FnMut() -> glib::ControlFlow + 'static,
    ) {
        let ended = self.start();
        let id = glib::timeout_add_local(interval, move || {
            ended.keep();
            run(what, &mut f)
        });
        self.inner.id.set(Some(SlotId::Source(id)));
    }

    /// [`timeout_local_once`] in place of the running one; the slot is
    /// empty while `f` runs (it may start the next one).
    pub fn timeout_once(&self, interval: Duration, what: &'static str, f: impl FnOnce() + 'static) {
        let ended = self.start();
        let id = glib::timeout_add_local_once(interval, move || {
            drop(ended);
            run(what, f)
        });
        self.inner.id.set(Some(SlotId::Source(id)));
    }
}

impl Drop for Slot {
    /// Its owner went: so does its callback.
    fn drop(&mut self) {
        self.stop();
    }
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
                offenders.extend(unguarded(&relative, &production(&text)));
            }
        }
        assert!(offenders.is_empty(), "unguarded callbacks: {offenders:#?}");
    }

    /// `text` without its `#[cfg(test)]` items: each attribute and the item
    /// it is on, to that item's matching `}` (or its `;`), wherever it is in
    /// the file (a test-only helper above production code included).
    fn production(text: &str) -> String {
        const ATTR: &str = "#[cfg(test)]";
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(ATTR) {
            out.push_str(&rest[..at]);
            let item = &rest[at + ATTR.len()..];
            rest = match item_end(item) {
                Some(end) => &item[end..],
                None => "",
            };
        }
        out.push_str(rest);
        out
    }

    /// Where the item starting in `text` ends: past the `}` closing its
    /// first `{`, or past a `;` before any `{` (string and char literals
    /// skipped).
    fn item_end(text: &str) -> Option<usize> {
        let mut depth = 0usize;
        let mut chars = Literals::new(text);
        while let Some((i, c)) = chars.next_code() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                ';' if depth == 0 => return Some(i + 1),
                _ => {}
            }
        }
        None
    }

    /// The characters of Rust code outside comments and string and char
    /// literals.
    struct Literals<'a> {
        chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    }

    impl<'a> Literals<'a> {
        fn new(text: &'a str) -> Self {
            Self {
                chars: text.char_indices().peekable(),
            }
        }

        /// The next character of code (a literal is skipped whole).
        fn next_code(&mut self) -> Option<(usize, char)> {
            loop {
                let (i, c) = self.chars.next()?;
                match c {
                    '"' => self.skip_string(),
                    // A comment (its quotes and apostrophes are prose).
                    '/' if matches!(self.chars.peek(), Some((_, '/'))) => {
                        for (_, c) in self.chars.by_ref() {
                            if c == '\n' {
                                break;
                            }
                        }
                    }
                    '/' if matches!(self.chars.peek(), Some((_, '*'))) => {
                        self.chars.next();
                        let mut star = false;
                        for (_, c) in self.chars.by_ref() {
                            if star && c == '/' {
                                break;
                            }
                            star = c == '*';
                        }
                    }
                    // A raw string: `r"…"`, `r#"…"#`.
                    'r' if self.raw_string_ahead().is_some() => {
                        let hashes = self.raw_string_ahead().unwrap_or(0);
                        for _ in 0..=hashes {
                            self.chars.next();
                        }
                        self.skip_raw_string(hashes);
                    }
                    // A char literal (`'x'`, `'\n'`); otherwise a lifetime.
                    '\'' => {
                        let mut ahead = self.chars.clone();
                        let char_literal = match ahead.next() {
                            Some((_, '\\')) => true,
                            Some(_) => matches!(ahead.next(), Some((_, '\''))),
                            None => false,
                        };
                        if char_literal {
                            while let Some((_, c)) = self.chars.next() {
                                match c {
                                    '\\' => {
                                        self.chars.next();
                                    }
                                    '\'' => break,
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => return Some((i, c)),
                }
            }
        }

        fn skip_string(&mut self) {
            while let Some((_, c)) = self.chars.next() {
                match c {
                    '\\' => {
                        self.chars.next();
                    }
                    '"' => return,
                    _ => {}
                }
            }
        }

        /// After an `r`: the `#`s of a raw string's opening, if one starts.
        fn raw_string_ahead(&self) -> Option<usize> {
            let mut ahead = self.chars.clone();
            let mut hashes = 0;
            loop {
                match ahead.next()? {
                    (_, '#') => hashes += 1,
                    (_, '"') => return Some(hashes),
                    _ => return None,
                }
            }
        }

        /// To the `"` and `hashes` `#`s that close a raw string.
        fn skip_raw_string(&mut self, hashes: usize) {
            while let Some((_, c)) = self.chars.next() {
                if c == '"' {
                    let mut ahead = self.chars.clone();
                    if (0..hashes).all(|_| matches!(ahead.next(), Some((_, '#')))) {
                        for _ in 0..hashes {
                            self.chars.next();
                        }
                        return;
                    }
                }
            }
        }
    }

    /// The top-level arguments of the call whose `(` is just before
    /// `args`, each trimmed; `None` when the call doesn't close.
    fn call_args(args: &str) -> Option<Vec<&str>> {
        let mut depth = 1usize;
        let mut start = 0;
        let mut out = Vec::new();
        let mut chars = Literals::new(args);
        while let Some((i, c)) = chars.next_code() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        // A trailing comma leaves nothing after it.
                        let last = args[start..i].trim();
                        if !last.is_empty() || out.is_empty() {
                            out.push(last);
                        }
                        return Some(out);
                    }
                }
                ',' if depth == 1 => {
                    out.push(args[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        None
    }

    /// The unguarded callbacks in `code` (comments stripped): GLib's source
    /// functions outside guard.rs, and every `.connect_…(`, `.launch(` and
    /// `.open_containing_folder(` call whose last argument isn't a
    /// `guarded!(…)` (a fn path, a variable, a closure of its own).
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
                let args = call_args(args);
                // `.connect_…()` without arguments is a method of ours that
                // wires things up (a signal connection takes its callback).
                if marker == ".connect_" && args.as_deref() == Some(&[""][..]) {
                    continue;
                }
                let guarded = args.and_then(|a| a.last().copied()).is_some_and(|last| {
                    ["guarded!(", "crate::guarded!(", "$crate::guarded!("]
                        .iter()
                        .any(|g| last.starts_with(g))
                });
                if !guarded {
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
                    launcher.launch(None, NONE, guarded!(\"open\", move |r| f(r)));\n\
                    w.connect_notify(Some(\"caps\"), crate::guarded!(\"caps\", move |p, _| f(p, ')')));";
        assert!(
            unguarded("x.rs", good).is_empty(),
            "{:?}",
            unguarded("x.rs", good)
        );
        assert!(unguarded("guard.rs", "glib::idle_add_full(p, f)").is_empty());
        // Our own wiring methods named `connect_…` take no callback.
        assert!(
            unguarded(
                "x.rs",
                "self.connect_drop();\nself.connect_canvas_input( );"
            )
            .is_empty()
        );
        let raw = "x.connect_clicked(guarded!(r#\"a)\"b\"#, move |_| f(')')));";
        assert!(unguarded("x.rs", raw).is_empty(), "{raw}");
        // Each of these, followed by a guarded connect (whose `guarded!(`
        // the old grep took for theirs), is still found.
        let after = "\nb.connect_clicked(guarded!(\"ok\", move |_| ok()));";
        for unguarded_call in [
            "w.connect_destroy(Self::on_destroy);",
            "a.connect_clicked(|b| b.grab_focus());",
            "let h = move |_| go(); a.connect_clicked(h);",
            "a.connect_clicked(move |_| go(\"guarded!(\"));",
        ] {
            let code = format!("{unguarded_call}{after}");
            assert_eq!(unguarded("x.rs", &code).len(), 1, "{code}");
        }
        // A test-only item above production code hides nothing below it.
        let file = "impl X {\n    #[cfg(test)]\n    fn helper(&self) -> u32 { 1 }\n}\n\
                    fn real(g: &gtk::GestureClick) { g.connect_pressed(move |_, _, _, _| go()); }\n\
                    #[cfg(test)]\nmod tests { fn t() { b.connect_clicked(move |_| ()); } }\n";
        let code = production(file);
        assert!(!code.contains("fn helper"), "{code}");
        assert!(code.contains("fn real"), "{code}");
        assert!(!code.contains("mod tests"), "{code}");
        assert_eq!(unguarded("x.rs", &code).len(), 1, "{code}");
    }
}
