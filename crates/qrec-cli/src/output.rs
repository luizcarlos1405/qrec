//! Where output goes (SPEC §14 output contract, A§4.9).
//!
//! - stdout: human results, or under `--json` exactly one `{"ok":…}`
//!   object, preceded by `{"event":"progress",…}` lines for streaming
//!   commands.
//! - stderr: errors, notices (`qrec: …`) and the export progress bar for
//!   humans; under `--json` only `{"event":"notice",…}` lines.
//!
//! Writes never panic: a closed stdout (`qrec clip list | head -1`) stops
//! further output quietly and the command still finishes (A§8.6).

use std::fmt::Write as _;
use std::io::{self, Write};

use clap::error::ErrorKind;
use qrec_core::CoreError;
use qrec_core::protocol::{Progress, ProgressPhase, Toast, ToastLevel};
use serde::Serialize;
use serde_json::Value;

/// `{"ok":true,"data":…}`.
#[derive(Serialize)]
struct WireOk<'a, T: Serialize> {
    ok: bool,
    data: &'a T,
}

/// `{"ok":false,"error":{"code","message","hint"?,"details"?}}`.
#[derive(Serialize)]
struct WireErr<'a> {
    ok: bool,
    error: &'a CoreError,
}

/// `{"ok":true,"data":data}` as one line.
pub(crate) fn ok_line<T: Serialize>(data: &T) -> String {
    json_text(&WireOk { ok: true, data })
}

/// `{"ok":false,"error":error}` as one line.
pub(crate) fn err_line(error: &CoreError) -> String {
    json_text(&WireErr { ok: false, error })
}

/// Writes `{"ok":false,"error":error}` and a newline.
pub(crate) fn write_err(out: &mut dyn Write, error: &CoreError) -> io::Result<()> {
    write_text_line(out, &err_line(error))
}

fn json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        // Every value printed here is plain data; a failure is a bug, and it
        // still has to come out as one valid JSON object.
        let error = CoreError::internal(format!("can't serialize the output: {e}"));
        format!(
            r#"{{"ok":false,"error":{}}}"#,
            serde_json::to_string(&error).unwrap_or_else(|_| "{}".into())
        )
    })
}

fn write_text_line(out: &mut dyn Write, line: &str) -> io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// `{"event":<event>,…fields of value}`: `value` must serialize as an
/// object; its fields keep their order, after `event`.
fn event_line<T: Serialize>(event: &str, value: &T) -> String {
    let fields = serde_json::to_string(value).unwrap_or_default();
    let rest = fields
        .strip_prefix('{')
        .filter(|rest| rest.ends_with('}'))
        .unwrap_or("}");
    if rest == "}" {
        format!(r#"{{"event":"{event}"}}"#)
    } else {
        format!(r#"{{"event":"{event}",{rest}"#)
    }
}

/// `{"event":"progress","phase":"render","done":…,"total":…,"etaSecs":…,"label":…}`.
pub(crate) fn progress_line(progress: &Progress) -> String {
    event_line("progress", progress)
}

/// `{"event":"notice","level":…,"title":…,"body":…,"actions":[…]}`.
pub(crate) fn notice_line(toast: &Toast) -> String {
    event_line("notice", toast)
}

/// One line describing a clap error for `--json`: the error, its tips and
/// the usage line, without colors or the "For more information" footer.
pub(crate) fn clap_error_message(err: &clap::Error) -> String {
    let text = err.render().to_string();
    let mut main: Vec<&str> = Vec::new();
    let mut tips: Vec<&str> = Vec::new();
    let mut usage: Vec<&str> = Vec::new();
    let mut in_main = true;
    let mut in_usage = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_main = false;
            in_usage = false;
            continue;
        }
        if in_main {
            main.push(trimmed.strip_prefix("error: ").unwrap_or(trimmed));
        } else if let Some(rest) = trimmed.strip_prefix("Usage: ") {
            usage.push(rest);
            in_usage = true;
        } else if in_usage {
            usage.push(trimmed);
        } else if let Some(tip) = trimmed.strip_prefix("tip: ") {
            tips.push(tip);
        }
    }
    let mut message = main.join(" ");
    for tip in tips {
        message.push_str("; ");
        message.push_str(tip);
    }
    if !usage.is_empty() {
        message.push_str(" (usage: ");
        message.push_str(&usage.join(" | "));
        message.push(')');
    }
    message
}

/// Whether clap printed help instead of an error because a command group
/// was given without a subcommand.
pub(crate) fn is_help_on_missing(err: &clap::Error) -> bool {
    err.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
}

// ------------------------------------------------------------------ streams

/// A stream that stops writing after its first failure (a closed pipe):
/// output is best effort, never a panic.
pub(crate) struct Stream<'a> {
    inner: &'a mut dyn Write,
    broken: bool,
}

impl<'a> Stream<'a> {
    pub(crate) fn new(inner: &'a mut dyn Write) -> Self {
        Self {
            inner,
            broken: false,
        }
    }

    /// Writes `text` as is and flushes.
    pub(crate) fn write(&mut self, text: &str) {
        if self.broken {
            return;
        }
        let result = self
            .inner
            .write_all(text.as_bytes())
            .and_then(|()| self.inner.flush());
        if result.is_err() {
            self.broken = true;
        }
    }

    /// Writes `text` and a newline.
    pub(crate) fn line(&mut self, text: &str) {
        let mut line = String::with_capacity(text.len() + 1);
        line.push_str(text);
        line.push('\n');
        self.write(&line);
    }
}

/// The CLI's view of stdout and stderr for one run.
pub(crate) struct Ui<'a> {
    out: Stream<'a>,
    err: Stream<'a>,
    /// `--json`.
    pub json: bool,
    /// `-q`: no notices or progress, no success messages for commands.
    pub quiet: bool,
    /// `-v`, `-vv`.
    pub verbose: u8,
    /// stderr is a terminal: the progress bar updates in place.
    err_tty: bool,
    bar: Bar,
    /// The last progress line printed under `--json` (repeats are dropped).
    last_progress: Option<String>,
}

impl<'a> Ui<'a> {
    pub(crate) fn new(
        out: &'a mut dyn Write,
        err: &'a mut dyn Write,
        json: bool,
        quiet: bool,
        verbose: u8,
        err_tty: bool,
    ) -> Self {
        Self {
            out: Stream::new(out),
            err: Stream::new(err),
            json,
            quiet,
            verbose,
            err_tty,
            bar: Bar::default(),
            last_progress: None,
        }
    }

    /// A line of human output on stdout.
    pub(crate) fn print(&mut self, text: &str) {
        self.end_progress();
        self.out.line(text);
    }

    /// Raw text on stdout (scripts, schemas, guides): a newline is added
    /// when missing.
    pub(crate) fn print_text(&mut self, text: &str) {
        self.end_progress();
        if text.ends_with('\n') {
            self.out.write(text);
        } else {
            self.out.line(text);
        }
    }

    /// The `--json` success object.
    pub(crate) fn print_ok(&mut self, data: &Value) {
        self.end_progress();
        self.out.line(&ok_line(data));
    }

    /// Reports `error` and returns its exit code: under `--json` the
    /// `{"ok":false,…}` object on stdout (stderr stays JSON lines only),
    /// else the message, its details and hint on stderr.
    pub(crate) fn fail(&mut self, error: &CoreError) -> u8 {
        self.end_progress();
        if self.json {
            self.out.line(&err_line(error));
        } else {
            self.err.write(&human_error(error, self.verbose));
        }
        error.exit_code()
    }

    /// A toast for people: `qrec: …` on stderr, or a notice JSON line on
    /// stderr under `--json`. `-q` drops all but warnings and errors.
    pub(crate) fn notice(&mut self, toast: &Toast) {
        if self.quiet && matches!(toast.level, ToastLevel::Info | ToastLevel::Success) {
            return;
        }
        self.end_progress();
        if self.json {
            self.err.line(&notice_line(toast));
        } else {
            self.err.write(&human_notice(toast));
        }
    }

    /// An informational notice (`qrec: using project …`).
    pub(crate) fn info(&mut self, title: impl Into<String>) {
        self.notice(&Toast::new(ToastLevel::Info, title));
    }

    /// Details for `-v` on stderr (never under `--json`).
    pub(crate) fn detail(&mut self, text: &str) {
        if self.verbose > 0 && !self.json {
            self.end_progress();
            self.err.line(&format!("qrec: {text}"));
        }
    }

    /// Progress of a streaming command: a JSON line on stdout under
    /// `--json`, else the progress bar on stderr.
    pub(crate) fn progress(&mut self, progress: &Progress) {
        if self.quiet {
            return;
        }
        if self.json {
            // ffmpeg reports the same frame count a few times while its
            // encoder fills up: one line per change is enough.
            let line = progress_line(progress);
            if self.last_progress.as_deref() != Some(line.as_str()) {
                self.out.line(&line);
                self.last_progress = Some(line);
            }
            return;
        }
        if let Some(text) = self.bar.update(progress, self.err_tty) {
            self.err.write(&text);
        }
    }

    /// Ends an in-place progress bar before other output.
    pub(crate) fn end_progress(&mut self) {
        if let Some(text) = self.bar.end(self.err_tty) {
            self.err.write(&text);
        }
    }
}

/// An error for people: the message (exactly, e.g. the folder rule's),
/// its structured details, then `hint: …`.
pub(crate) fn human_error(error: &CoreError, verbose: u8) -> String {
    let mut text = String::new();
    text.push_str(&error.message);
    text.push('\n');
    if let Some(details) = &error.details {
        for line in details_lines(details, verbose) {
            let _ = writeln!(text, "  {line}");
        }
    }
    if let Some(hint) = &error.hint {
        let _ = writeln!(text, "hint: {hint}");
    }
    text
}

/// The structured `details` of an error (A§4.8) as lines: files and sizes,
/// history entries lost, re-quantized and removed clips; anything else as
/// compact JSON with `-v`.
pub(crate) fn details_lines(details: &Value, verbose: u8) -> Vec<String> {
    let mut lines = Vec::new();
    let mut known = false;
    if let Some(files) = details.get("files").and_then(Value::as_array) {
        known = true;
        let rows: Vec<Vec<String>> = files
            .iter()
            .map(|f| {
                vec![
                    f.get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    f.get("bytes")
                        .and_then(Value::as_u64)
                        .map(human_bytes)
                        .unwrap_or_default(),
                ]
            })
            .collect();
        lines.extend(crate::table::layout(
            &[],
            &rows,
            &[crate::table::Align::Left, crate::table::Align::Right],
        ));
    }
    if let Some(lost) = details.get("historyEntriesLost").and_then(Value::as_u64) {
        known = true;
        let mut line = format!("history entries lost: {lost}");
        if let Some(oldest) = details.get("oldestKept").and_then(Value::as_str) {
            let _ = write!(line, " (oldest kept: {oldest})");
        }
        lines.push(line);
    }
    if let Some(clips) = details.get("clips").and_then(Value::as_array) {
        known = true;
        for clip in clips {
            let id = clip.get("id").and_then(Value::as_str).unwrap_or("?");
            let range = |v: Option<&Value>| match v {
                Some(Value::Array(pair)) if pair.len() == 2 => {
                    format!("[{}, {})", pair[0], pair[1])
                }
                Some(Value::Object(o)) => format!(
                    "[{}, {})",
                    o.get("in").map(Value::to_string).unwrap_or_default(),
                    o.get("out").map(Value::to_string).unwrap_or_default()
                ),
                Some(other) => other.to_string(),
                None => "?".into(),
            };
            lines.push(format!(
                "{id}: {} -> {}",
                range(clip.get("before")),
                range(clip.get("after"))
            ));
        }
    }
    if let Some(removed) = details.get("removedClips").and_then(Value::as_array) {
        known = true;
        if !removed.is_empty() {
            let ids: Vec<String> = removed
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .collect();
            lines.push(format!("clips removed: {}", ids.join(", ")));
        }
    }
    if !known && verbose > 0 {
        lines.push(details.to_string());
    }
    lines
}

/// A toast for people on stderr: `qrec: <title>`, then its body indented.
pub(crate) fn human_notice(toast: &Toast) -> String {
    let level = match toast.level {
        ToastLevel::Info | ToastLevel::Success => "",
        ToastLevel::Warning => "warning: ",
        ToastLevel::Error => "error: ",
    };
    let mut text = format!("qrec: {level}{}\n", toast.title);
    if let Some(body) = &toast.body {
        for line in body.lines() {
            let _ = writeln!(text, "  {line}");
        }
    }
    text
}

/// `412 B`, `12.3 KB`, `1.2 GB` (SI units, like file managers).
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

// ------------------------------------------------------------- progress bar

/// The human progress line: in place on a terminal, else one line per
/// phase change and per 10 %.
#[derive(Default)]
struct Bar {
    /// An in-place line is on screen.
    active: bool,
    /// What the last printed line showed: phase, label, percent bucket.
    last: Option<(ProgressPhase, String, u64)>,
}

/// The longest progress line: stays on one line of an 80-column terminal.
const BAR_WIDTH: usize = 79;
/// Cells of the `[####....]` bar.
const BAR_CELLS: u64 = 20;

impl Bar {
    /// The text to write for `progress`, if any.
    fn update(&mut self, progress: &Progress, tty: bool) -> Option<String> {
        let line = progress_text(progress);
        if tty {
            self.active = true;
            return Some(format!("\r\x1b[2K{line}"));
        }
        let bucket = percent(progress).map_or(0, |p| p / 10);
        let key = (progress.phase, progress.label.clone(), bucket);
        if self.last.as_ref() == Some(&key) {
            return None;
        }
        self.last = Some(key);
        Some(format!("{line}\n"))
    }

    /// Clears the in-place line.
    fn end(&mut self, tty: bool) -> Option<String> {
        self.last = None;
        if tty && std::mem::take(&mut self.active) {
            Some("\r\x1b[2K".to_owned())
        } else {
            None
        }
    }
}

fn percent(progress: &Progress) -> Option<u64> {
    (progress.total > 0).then(|| progress.done.min(progress.total) * 100 / progress.total)
}

/// `Rendering clip 4/12 [########............]  43%  ETA 0:42`: label (or
/// phase), bar and percent when the total is known, ETA when estimated.
pub(crate) fn progress_text(progress: &Progress) -> String {
    let label = if progress.label.trim().is_empty() {
        phase_name(progress.phase).to_owned()
    } else {
        progress.label.trim().to_owned()
    };
    let mut tail = String::new();
    if let Some(pct) = percent(progress) {
        let filled = (pct * BAR_CELLS / 100) as usize;
        let _ = write!(
            tail,
            " [{}{}] {pct:>3}%",
            "#".repeat(filled),
            ".".repeat(BAR_CELLS as usize - filled)
        );
    }
    if let Some(eta) = progress.eta_secs {
        let _ = write!(tail, "  ETA {}", clock(eta));
    }
    let room = BAR_WIDTH.saturating_sub(tail.chars().count());
    let label: String = if label.chars().count() > room {
        let keep = room.saturating_sub(1);
        label
            .chars()
            .take(keep)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        label
    };
    format!("{label}{tail}")
}

fn phase_name(phase: ProgressPhase) -> &'static str {
    match phase {
        ProgressPhase::Analyze => "Analyzing",
        ProgressPhase::Render => "Rendering",
        ProgressPhase::Stitch => "Stitching",
        ProgressPhase::Record => "Recording",
        ProgressPhase::Finalize => "Finalizing",
    }
}

/// `42s`, `1:02`, `1:02:03`.
pub(crate) fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if m > 0 {
        format!("{m}:{s:02}")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use qrec_core::protocol::UiAction;
    use serde_json::json;

    use super::*;

    fn progress(
        phase: ProgressPhase,
        done: u64,
        total: u64,
        eta: Option<u64>,
        label: &str,
    ) -> Progress {
        Progress {
            phase,
            done,
            total,
            eta_secs: eta,
            label: label.into(),
            take: None,
        }
    }

    #[test]
    fn json_lines() {
        let p = progress(
            ProgressPhase::Render,
            1234,
            5678,
            Some(42),
            "Rendering clip 4/12",
        );
        assert_eq!(
            progress_line(&p),
            r#"{"event":"progress","phase":"render","done":1234,"total":5678,"etaSecs":42,"label":"Rendering clip 4/12"}"#
        );
        let toast = Toast::new(ToastLevel::Success, "Your video is ready.")
            .with_action("Copy path", UiAction::CopyText("/v/x.mp4".into()));
        let line = notice_line(&toast);
        assert!(
            line.starts_with(
                r#"{"event":"notice","level":"success","title":"Your video is ready.""#
            ),
            "{line}"
        );
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value["actions"][0]["action"],
            json!({"copyText": "/v/x.mp4"})
        );
        assert_eq!(ok_line(&json!([1])), r#"{"ok":true,"data":[1]}"#);
        assert_eq!(
            err_line(&CoreError::not_found("no clip c1")),
            r#"{"ok":false,"error":{"code":"not_found","message":"no clip c1"}}"#
        );
    }

    #[test]
    fn progress_text_shows_phase_percent_and_eta() {
        assert_eq!(
            progress_text(&progress(
                ProgressPhase::Render,
                43,
                100,
                Some(42),
                "Rendering clip 4/12"
            )),
            "Rendering clip 4/12 [########............]  43%  ETA 42s"
        );
        assert_eq!(
            progress_text(&progress(ProgressPhase::Stitch, 10, 10, Some(3725), "")),
            "Stitching [####################] 100%  ETA 1:02:05"
        );
        assert_eq!(
            progress_text(&progress(
                ProgressPhase::Record,
                0,
                0,
                None,
                "Recording take 3 · 00:00:12"
            )),
            "Recording take 3 · 00:00:12"
        );
        let long = "x".repeat(200);
        let text = progress_text(&progress(ProgressPhase::Analyze, 1, 2, Some(61), &long));
        assert_eq!(text.chars().count(), BAR_WIDTH);
        assert!(text.ends_with("  50%  ETA 1:01"), "{text}");
        assert_eq!(clock(59), "59s");
        assert_eq!(clock(60), "1:00");
    }

    #[test]
    fn the_bar_updates_in_place_on_a_terminal() {
        let mut bar = Bar::default();
        let p = progress(ProgressPhase::Render, 1, 4, None, "Rendering");
        assert_eq!(
            bar.update(&p, true).unwrap(),
            "\r\x1b[2KRendering [#####...............]  25%"
        );
        assert_eq!(bar.end(true).unwrap(), "\r\x1b[2K");
        assert_eq!(bar.end(true), None, "nothing left to clear");

        // Without a terminal: a line per 10 % step or label change.
        let mut lines = Vec::new();
        for done in [0, 1, 5, 10, 11, 100] {
            if let Some(text) = bar.update(
                &progress(ProgressPhase::Render, done, 100, None, "R"),
                false,
            ) {
                lines.push(text);
            }
        }
        assert_eq!(
            lines,
            [
                "R [....................]   0%\n",
                "R [##..................]  10%\n",
                "R [####################] 100%\n"
            ]
        );
        assert_eq!(bar.end(false), None);
    }

    #[test]
    fn errors_for_people() {
        let error = CoreError::confirmation_required("purge take-0003 (2 files, 412.0 MB)")
            .with_details(json!({
                "files": [
                    {"path": "takes/take-0003/L1-screen-DP-4.mkv", "bytes": 400_000_000u64},
                    {"path": "takes/take-0003/A1-mic.mka", "bytes": 12_000_000u64}
                ],
                "historyEntriesLost": 3,
                "oldestKept": "Split clip c7f3"
            }));
        assert_eq!(
            human_error(&error, 0),
            "confirmation required: purge take-0003 (2 files, 412.0 MB)\n\
             \x20 takes/take-0003/L1-screen-DP-4.mkv  400.0 MB\n\
             \x20 takes/take-0003/A1-mic.mka           12.0 MB\n\
             \x20 history entries lost: 3 (oldest kept: Split clip c7f3)\n\
             hint: pass --yes to confirm\n"
        );
        let fps = CoreError::confirmation_required("re-quantize 2 clips").with_details(json!({
            "clips": [{"id": "c1", "before": [0, 30], "after": [0, 25]}],
            "removedClips": ["c2"]
        }));
        assert_eq!(
            details_lines(fps.details.as_ref().unwrap(), 0),
            ["c1: [0, 30) -> [0, 25)", "clips removed: c2"]
        );
        let odd = json!({"why": 1});
        assert!(details_lines(&odd, 0).is_empty());
        assert_eq!(details_lines(&odd, 1), [r#"{"why":1}"#]);
    }

    #[test]
    fn notices_and_bytes() {
        let toast = Toast::new(ToastLevel::Warning, "record-over target changed")
            .with_body("take appended at the end");
        assert_eq!(
            human_notice(&toast),
            "qrec: warning: record-over target changed\n  take appended at the end\n"
        );
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1000), "1.0 KB");
        assert_eq!(human_bytes(12_345_678), "12.3 MB");
        assert_eq!(human_bytes(999_960_000), "1.0 GB");
    }

    #[test]
    fn closed_streams_stop_quietly() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (mut out, mut err) = (Closed, Vec::new());
        let mut ui = Ui::new(&mut out, &mut err, false, false, 0, false);
        ui.print("one");
        ui.print("two");
        assert!(ui.out.broken);
        assert_eq!(ui.fail(&CoreError::busy("recording")), 6);
        drop(ui);
        assert_eq!(String::from_utf8(err).unwrap(), "recording\n");
    }

    #[test]
    fn json_mode_keeps_stderr_json() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut ui = Ui::new(&mut out, &mut err, true, false, 1, false);
        ui.info("using project /v (pid 4)");
        ui.detail("routed to pid 4");
        ui.progress(&progress(ProgressPhase::Render, 1, 2, None, "R"));
        assert_eq!(ui.fail(&CoreError::not_found("no clip c1")), 7);
        drop(ui);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"event\":\"progress\",\"phase\":\"render\",\"done\":1,\"total\":2,\"label\":\"R\"}\n\
             {\"ok\":false,\"error\":{\"code\":\"not_found\",\"message\":\"no clip c1\"}}\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "{\"event\":\"notice\",\"level\":\"info\",\"title\":\"using project /v (pid 4)\",\"body\":null,\"actions\":[]}\n"
        );
    }

    #[test]
    fn quiet_drops_chatter_but_not_warnings() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut ui = Ui::new(&mut out, &mut err, false, true, 0, true);
        ui.info("using project /v (pid 4)");
        ui.progress(&progress(ProgressPhase::Render, 1, 2, None, "R"));
        ui.notice(&Toast::new(ToastLevel::Warning, "careful"));
        drop(ui);
        assert_eq!(String::from_utf8(err).unwrap(), "qrec: warning: careful\n");
        assert!(out.is_empty());
    }
}
