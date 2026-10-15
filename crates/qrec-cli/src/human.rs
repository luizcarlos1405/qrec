//! Human renderings of replies: short lines and aligned tables (A§10).
//! `--json` prints the replies themselves; these functions only shape text.
//!
//! Frame counts carry an `f` (`410f`), the unit time arguments take, so a
//! value can be pasted into the next command.

use std::fmt::Write as _;

use qrec_core::keymap::{KeyChord, Keymap};
use qrec_core::protocol::{
    AutocutPlanView, ClipRow, HelloInfo, HistoryRow, RecordStartedView, StatusView,
    TakeFinalizedView, TakeRow,
};
use qrec_core::state::{Job, JobKind, RecordMode, RecordPhase};
use qrec_core::time::duration_label;
use serde_json::Value;

use crate::output::human_bytes;
use crate::table::{Align, layout};

/// `410f`.
fn frames(n: u64) -> String {
    format!("{n}f")
}

/// Where a request ran, for `status`.
pub(crate) fn host_line(host: Option<&HelloInfo>, view_pid: Option<u32>) -> String {
    match host {
        Some(hello) => format!("{} (pid {})", hello.host.as_str(), hello.pid),
        None => format!(
            "none running; answered by this process (pid {})",
            view_pid.unwrap_or_else(std::process::id)
        ),
    }
}

/// `qrec status` for people.
pub(crate) fn status(view: &StatusView, dir: &str, host: Option<&HelloInfo>) -> Vec<String> {
    let path = view.path.as_deref().unwrap_or(dir);
    let mut rows = vec![
        kv("Project", &view.project),
        kv("Folder", path),
        kv("Host", &host_line(host, view.pid)),
        kv("Recording", &record_phase(&view.record)),
    ];
    let jobs: Vec<String> = view.jobs.iter().map(job).collect();
    rows.push(kv(
        "Jobs",
        &if jobs.is_empty() {
            "none".to_owned()
        } else {
            jobs.join("; ")
        },
    ));
    rows.push(kv(
        "Timeline",
        &format!(
            "{} clip{}, {}",
            view.clips,
            plural(view.clips),
            frames(view.duration_frames)
        ),
    ));
    rows.push(kv("Takes", &view.takes.to_string()));
    rows.push(kv("History", &format!("at seq {}", view.cursor)));
    layout(&[], &rows, &[])
}

/// `qrec status` in an empty folder that isn't a project yet.
pub(crate) fn status_empty(dir: &str) -> Vec<String> {
    let rows = vec![
        kv("Project", "none yet"),
        kv("Folder", dir),
        kv(
            "Note",
            "this empty folder becomes a project with `qrec init` or on the first change",
        ),
    ];
    layout(&[], &rows, &[])
}

fn kv(key: &str, value: &str) -> Vec<String> {
    vec![key.to_owned(), value.to_owned()]
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `idle`, `countdown 3 s`, `recording take-0003 (1 min 4.0 s)`, …
pub(crate) fn record_phase(phase: &RecordPhase) -> String {
    let over = |mode: &RecordMode| match mode {
        RecordMode::Append => String::new(),
        RecordMode::Over { clips } => {
            let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
            format!(", over {}", ids.join(", "))
        }
    };
    match phase {
        RecordPhase::Idle => "idle".to_owned(),
        RecordPhase::Countdown {
            remaining, mode, ..
        } => format!("countdown {remaining} s{}", over(mode)),
        RecordPhase::Starting { take, mode, .. } => format!("starting {take}{}", over(mode)),
        RecordPhase::Recording {
            take,
            mode,
            elapsed_ns,
            ..
        } => format!(
            "recording {take} ({}{})",
            duration_label(i64::try_from(*elapsed_ns).unwrap_or(i64::MAX)),
            over(mode)
        ),
        RecordPhase::Finalizing {
            take, cancelled, ..
        } => format!(
            "finalizing {take}{}",
            if *cancelled { " (cancelled)" } else { "" }
        ),
    }
}

/// One running job: `export to /v/out.mp4 (43 %)`.
pub(crate) fn job(job: &Job) -> String {
    match &job.kind {
        JobKind::Autocut {
            targets,
            pending,
            dry_run,
            ..
        } => format!(
            "{} of {} clip{} ({} file{} to analyze)",
            if *dry_run {
                "autocut preview"
            } else {
                "autocut"
            },
            targets.len(),
            plural(targets.len()),
            pending.len(),
            plural(pending.len())
        ),
        JobKind::Export {
            out,
            total_frames,
            seg_done,
            stitch_total_ns,
            stitch_done_ns,
            ..
        } => {
            let rendered: u64 = seg_done.iter().sum();
            let render = if *total_frames > 0 {
                rendered.min(*total_frames) as f64 / *total_frames as f64
            } else {
                0.0
            };
            let stitch = if *stitch_total_ns > 0 {
                (*stitch_done_ns).min(*stitch_total_ns) as f64 / *stitch_total_ns as f64
            } else {
                0.0
            };
            let pct = ((0.9 * render + 0.1 * stitch) * 100.0).floor() as u64;
            format!("export to {out} ({pct} %)")
        }
        JobKind::FrameSave { out, .. } => format!("frame save to {out}"),
        JobKind::Purge { takes } => {
            let ids: Vec<&str> = takes.iter().map(|t| t.as_str()).collect();
            format!("purge of {}", ids.join(", "))
        }
    }
}

/// `qrec take list`.
pub(crate) fn take_list(rows: &[TakeRow]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["No takes.".to_owned()];
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|t| {
            let mut note = Vec::new();
            if t.unused {
                note.push("in the bin".to_owned());
            }
            if let Some(error) = &t.error {
                note.push(error.clone());
            }
            vec![
                t.id.to_string(),
                t.status.as_str().to_owned(),
                frames(t.duration_frames),
                t.clips.to_string(),
                t.recorded_at.format("%Y-%m-%d %H:%M:%SZ").to_string(),
                note.join(": "),
            ]
        })
        .collect();
    layout(
        &["TAKE", "STATUS", "LENGTH", "CLIPS", "RECORDED", "NOTE"],
        &table,
        &[Align::Left, Align::Left, Align::Right, Align::Right],
    )
}

/// `qrec take show`: the take's JSON (a `Take` plus `id`, `unused` and
/// `clips` as `ClipRow`s), read leniently.
pub(crate) fn take_show(take: &Value) -> Vec<String> {
    let s = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64);
    let mut lines = Vec::new();
    let mut head = format!(
        "{}  {}  {}",
        s(take, "id"),
        s(take, "status"),
        frames(n(take, "durationFrames").unwrap_or(0))
    );
    let recorded = s(take, "recordedAt");
    if !recorded.is_empty() {
        let _ = write!(head, "  recorded {recorded}");
    }
    lines.push(head);
    if let Some(error) = take.get("error").and_then(Value::as_str) {
        lines.push(format!("Error: {error}"));
    }
    let clips: Vec<String> = take
        .get("clips")
        .and_then(Value::as_array)
        .map(|clips| {
            clips
                .iter()
                .map(|c| {
                    format!(
                        "{} [{}, {}) at {}",
                        s(c, "id"),
                        n(c, "in").unwrap_or(0),
                        n(c, "out").unwrap_or(0),
                        frames(n(c, "start").unwrap_or(0))
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if clips.is_empty() {
        lines.push("Clips: none (the take is in the bin)".to_owned());
    } else {
        lines.push(format!("Clips: {}", clips.join(", ")));
    }
    let streams = take.get("streams").and_then(Value::as_array);
    if let Some(streams) = streams.filter(|s| !s.is_empty()) {
        lines.push("Streams".to_owned());
        let rows: Vec<Vec<String>> = streams.iter().map(|st| stream_row(st, &s, &n)).collect();
        lines.extend(
            layout(&[], &rows, &[])
                .into_iter()
                .map(|l| format!("  {l}")),
        );
    }
    if let Some(scene) = take.get("scene") {
        lines.push(format!("Scene: {}", scene_summary(scene)));
    }
    lines
}

fn stream_row(
    st: &Value,
    s: &dyn Fn(&Value, &str) -> String,
    n: &dyn Fn(&Value, &str) -> Option<u64>,
) -> Vec<String> {
    let mut detail = Vec::new();
    if let (Some(w), Some(h)) = (n(st, "width"), n(st, "height")) {
        detail.push(format!("{w}x{h}"));
    }
    if let Some(fps) = st.get("fps")
        && let (Some(num), Some(den)) = (n(fps, "num"), n(fps, "den"))
    {
        detail.push(if den == 1 {
            format!("{num} fps")
        } else {
            format!("{num}/{den} fps")
        });
    }
    if let Some(count) = n(st, "frames") {
        detail.push(format!("{count} frames"));
    }
    if let Some(rate) = n(st, "sampleRate") {
        detail.push(format!("{rate} Hz"));
    }
    if let Some(channels) = n(st, "channels") {
        detail.push(format!("{channels} ch"));
    }
    if let Some(offset) = st.get("startOffsetNs").and_then(Value::as_i64)
        && offset != 0
    {
        detail.push(format!("offset {:.1} ms", offset as f64 / 1e6));
    }
    vec![s(st, "id"), s(st, "kind"), s(st, "file"), detail.join(", ")]
}

/// `2 layers (L1 Screen DP-4, L2 Camera), 1 audio track (A1 Mic)`.
fn scene_summary(scene: &Value) -> String {
    let names = |key: &str, name: &str| -> Vec<String> {
        scene
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let id = item.get("id").and_then(Value::as_str).unwrap_or("?");
                        match item.get(name).and_then(Value::as_str) {
                            Some(label) if !label.is_empty() => format!("{id} {label}"),
                            _ => id.to_owned(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let part = |items: Vec<String>, what: &str| {
        if items.is_empty() {
            format!("no {what}s")
        } else {
            format!(
                "{} {what}{} ({})",
                items.len(),
                plural(items.len()),
                items.join(", ")
            )
        }
    };
    format!(
        "{}, {}",
        part(names("layers", "name"), "layer"),
        part(names("audio", "label"), "audio track")
    )
}

/// `qrec clip list`.
pub(crate) fn clip_list(rows: &[ClipRow]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["The timeline is empty.".to_owned()];
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .enumerate()
        .map(|(i, c)| {
            vec![
                i.to_string(),
                c.id.to_string(),
                c.take.to_string(),
                frames(c.in_),
                frames(c.out),
                frames(c.start),
                frames(c.duration),
            ]
        })
        .collect();
    let mut lines = layout(
        &["#", "CLIP", "TAKE", "IN", "OUT", "START", "LENGTH"],
        &table,
        &[
            Align::Right,
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    // Saturating: the rows come from a host, and a sum must not panic.
    let total = rows.iter().map(|c| c.duration).fold(0, u64::saturating_add);
    lines.push(format!(
        "{} clip{}, {}",
        rows.len(),
        plural(rows.len()),
        frames(total)
    ));
    lines
}

/// `qrec history`: oldest first, `*` on the current entry.
pub(crate) fn history(rows: &[HistoryRow]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["No history yet.".to_owned()];
    }
    let current = rows.iter().position(|r| r.current);
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                if r.current { "*" } else { "" }.to_owned(),
                r.seq.to_string(),
                r.ts.format("%Y-%m-%d %H:%M:%SZ").to_string(),
                r.label.clone(),
            ]
        })
        .collect();
    let mut lines = layout(
        &["", "SEQ", "WHEN", "CHANGE"],
        &table,
        &[Align::Left, Align::Right],
    );
    match current {
        Some(i) if i + 1 < rows.len() => {
            lines.push("* = the current state; the entries after it can be redone".to_owned())
        }
        None => lines.push("Everything listed is undone; `qrec redo` brings it back".to_owned()),
        _ => {}
    }
    lines
}

/// The cuts of `qrec autocut`: one row per clip, then the skipped clips.
pub(crate) fn autocut_plan(plan: &AutocutPlanView, dry_run: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if !plan.clips.is_empty() {
        let rows: Vec<Vec<String>> = plan
            .clips
            .iter()
            .map(|c| {
                let kept: Vec<String> = c.kept.iter().map(|[a, b]| format!("{a}-{b}")).collect();
                vec![
                    c.clip.to_string(),
                    c.silences.to_string(),
                    frames(c.removed_frames),
                    kept.join(" "),
                ]
            })
            .collect();
        lines.extend(layout(
            &["CLIP", "SILENCES", "REMOVED", "KEPT (take frames)"],
            &rows,
            &[Align::Left, Align::Right, Align::Right],
        ));
    }
    for skip in &plan.skipped {
        lines.push(format!("skipped {}: {}", skip.clip, skip.reason));
    }
    if dry_run {
        let silences: usize = plan.clips.iter().map(|c| c.silences).sum();
        let removed = plan
            .clips
            .iter()
            .map(|c| c.removed_frames)
            .fold(0, u64::saturating_add);
        lines.push(if silences == 0 {
            "Nothing to cut.".to_owned()
        } else {
            format!(
                "Would remove {silences} silence{} ({}). Run without --dry-run to cut.",
                plural(silences),
                frames(removed)
            )
        });
    }
    lines
}

/// The extra line of a finished `record start`: what is being written.
pub(crate) fn record_started(view: &RecordStartedView) -> Vec<String> {
    let mut lines = Vec::new();
    if !view.files.is_empty() {
        lines.push(format!("Writing {}", view.files.join(", ")));
    }
    lines.push("Stop with `qrec record stop` (or `qrec record cancel` to bin it).".to_owned());
    lines
}

/// The extra line of a finalized take.
pub(crate) fn take_finalized(view: &TakeFinalizedView) -> Vec<String> {
    if view.clips.is_empty() {
        return Vec::new();
    }
    let ids: Vec<&str> = view.clips.iter().map(|c| c.as_str()).collect();
    vec![format!("Clips: {}", ids.join(", "))]
}

/// `qrec sources list` from the `SourceInventory` JSON, read leniently.
/// A camera's modes in one short line: per format its largest mode and its
/// fastest one, largest first, at most four, then how many more there are
/// (`--json` lists them all). A Brio reports over 300.
fn camera_modes_summary(modes: &[Value]) -> String {
    const SHOWN: usize = 4;
    struct Mode {
        w: u64,
        h: u64,
        fps: f64,
        rate: String,
        format: String,
    }
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64);
    let mut all: Vec<Mode> = Vec::new();
    for m in modes {
        let fps = m.get("fps");
        let (num, den) = (fps.and_then(|f| n(f, "num")), fps.and_then(|f| n(f, "den")));
        let rate = match (num, den) {
            (Some(num), Some(1)) => num.to_string(),
            (Some(num), Some(den)) => format!("{num}/{den}"),
            _ => "?".into(),
        };
        let format = match m.get("format").and_then(Value::as_str).unwrap_or("") {
            "image/jpeg" => "MJPEG".to_owned(),
            other => other.to_owned(),
        };
        let mode = Mode {
            w: n(m, "width").unwrap_or(0),
            h: n(m, "height").unwrap_or(0),
            fps: match (num, den) {
                (Some(num), Some(den)) if den > 0 => num as f64 / den as f64,
                _ => 0.0,
            },
            rate,
            format,
        };
        let same = |o: &Mode| {
            o.w == mode.w && o.h == mode.h && o.rate == mode.rate && o.format == mode.format
        };
        if !all.iter().any(same) {
            all.push(mode);
        }
    }
    let area = |m: &Mode| m.w * m.h;
    let mut picked: Vec<&Mode> = Vec::new();
    let mut formats: Vec<&str> = all.iter().map(|m| m.format.as_str()).collect();
    formats.dedup();
    let mut seen_formats = Vec::new();
    for format in formats {
        if seen_formats.contains(&format) {
            continue;
        }
        seen_formats.push(format);
        let of = || all.iter().filter(|m| m.format == format);
        let largest = of().max_by(|a, b| area(a).cmp(&area(b)).then(a.fps.total_cmp(&b.fps)));
        let fastest = of().max_by(|a, b| a.fps.total_cmp(&b.fps).then(area(a).cmp(&area(b))));
        for m in [largest, fastest].into_iter().flatten() {
            if !picked.iter().any(|p| std::ptr::eq(*p, m)) {
                picked.push(m);
            }
        }
    }
    picked.sort_by(|a, b| area(b).cmp(&area(a)).then(b.fps.total_cmp(&a.fps)));
    let rest = all.len().saturating_sub(picked.len().min(SHOWN));
    let mut text: Vec<String> = picked
        .iter()
        .take(SHOWN)
        .map(|m| format!("{}x{}@{} {}", m.w, m.h, m.rate, m.format))
        .collect();
    if rest > 0 {
        text.push(format!("+{rest} more"));
    }
    text.join(", ")
}

pub(crate) fn sources(inv: &Value) -> Vec<String> {
    let s = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64);
    let list = |key: &str| {
        inv.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut lines = Vec::new();
    let mut section = |title: &str, headers: &[&str], rows: Vec<Vec<String>>| {
        lines.push(title.to_owned());
        if rows.is_empty() {
            lines.push("  none found".to_owned());
        } else {
            lines.extend(
                layout(headers, &rows, &[])
                    .into_iter()
                    .map(|l| format!("  {l}")),
            );
        }
    };
    let screens = list("screens")
        .iter()
        .map(|sc| {
            let scale = sc.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
            vec![
                s(sc, "output"),
                format!(
                    "{}x{}",
                    n(sc, "width").unwrap_or(0),
                    n(sc, "height").unwrap_or(0)
                ),
                format!(
                    "{},{}",
                    sc.get("x").and_then(Value::as_i64).unwrap_or(0),
                    sc.get("y").and_then(Value::as_i64).unwrap_or(0)
                ),
                format!("{scale}"),
                s(sc, "description"),
            ]
        })
        .collect();
    section(
        "Screens",
        &["OUTPUT", "SIZE", "POSITION", "SCALE", "DESCRIPTION"],
        screens,
    );
    let cameras = list("cameras")
        .iter()
        .map(|cam| {
            let modes = cam
                .get("modes")
                .and_then(Value::as_array)
                .map(|modes| camera_modes_summary(modes))
                .unwrap_or_default();
            vec![s(cam, "device"), s(cam, "label"), modes]
        })
        .collect();
    section("Cameras", &["DEVICE", "LABEL", "MODES"], cameras);
    let audio = |key: &str, default_key: &str| -> Vec<Vec<String>> {
        let default = inv
            .get(default_key)
            .and_then(Value::as_str)
            .map(str::to_owned);
        list(key)
            .iter()
            .map(|dev| {
                let name = s(dev, "name");
                let mark = if default.as_deref() == Some(name.as_str()) {
                    "yes"
                } else {
                    ""
                };
                vec![name, s(dev, "description"), mark.to_owned()]
            })
            .collect()
    };
    section(
        "Mics",
        &["NAME", "DESCRIPTION", "DEFAULT"],
        audio("mics", "defaultMic"),
    );
    section(
        "System audio",
        &["NAME", "DESCRIPTION", "DEFAULT"],
        audio("monitors", "defaultMonitor"),
    );
    lines
}

/// `qrec doctor` from the `DoctorReport` JSON: the lines and whether a
/// required check failed (exit 5).
pub(crate) fn doctor(report: &Value, verbose: u8) -> (Vec<String>, bool) {
    let checks = report
        .get("checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut failed_required = false;
    let mut rows = Vec::new();
    let mut hints = Vec::new();
    for check in &checks {
        let ok = check.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let required = check
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        failed_required |= required && !ok;
        let status = match (ok, required) {
            (true, _) => "ok",
            (false, true) => "MISSING",
            (false, false) => "missing (optional)",
        };
        let id = check
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let detail = check
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(hint) = check.get("hint").and_then(Value::as_str).filter(|_| !ok) {
            hints.push(format!("{id}: {hint}"));
        }
        rows.push(vec![status.to_owned(), id, detail]);
    }
    let mut lines = layout(&["STATUS", "CHECK", "DETAIL"], &rows, &[]);
    if !hints.is_empty() {
        lines.push(String::new());
        lines.push("How to fix:".to_owned());
        lines.extend(hints.into_iter().map(|h| format!("  {h}")));
    }
    if let Some(va) = report.get("va").filter(|v| !v.is_null()) {
        let device = va.get("device").and_then(Value::as_str);
        match device {
            Some(device) => lines.push(format!("\nVA-API device: {device}")),
            None if verbose > 0 => lines.push(format!("\nVA-API: {va}")),
            None => {}
        }
    }
    if let Some(snippet) = report.get("niriSnippet").and_then(Value::as_str) {
        lines.push(String::new());
        lines.push(
            "Record from any app with these niri key binds (~/.config/niri/config.kdl):".to_owned(),
        );
        lines.extend(snippet.trim_end().lines().map(str::to_owned));
    }
    lines.push(String::new());
    lines.push(if failed_required {
        "Some required pieces are missing: Qrec can't record or export until they are installed."
            .to_owned()
    } else {
        "Everything Qrec needs is here.".to_owned()
    });
    (lines, failed_required)
}

/// A chord in the `keys.toml` grammar: `ctrl+shift+z`, `less`, `Escape`.
pub(crate) fn chord(c: &KeyChord) -> String {
    let mut out = String::new();
    for (on, name) in [
        (c.ctrl, "ctrl+"),
        (c.alt, "alt+"),
        (c.shift, "shift+"),
        (c.super_, "super+"),
    ] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&c.key);
    out
}

/// `qrec keys --json`: `[{"id","group","label","keys":[..],"cli","contexts"}]`,
/// the core's own view of A§9.7 (also `Query::Keys`).
pub(crate) fn keys_json(map: &Keymap) -> Value {
    serde_json::to_value(qrec_core::keymap::keys_view(map)).unwrap_or(Value::Null)
}

/// What is wrong with a keymap: conflicts, then `keys.toml` errors.
pub(crate) fn key_problems(map: &Keymap) -> Vec<String> {
    let conflicts = map.conflicts.iter().map(|c| {
        let context = serde_json::to_value(c.context)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        format!(
            "{} is bound to {} in {context}; neither runs until keys.toml changes one",
            chord(&c.chord),
            c.ids.join(" and ")
        )
    });
    conflicts.chain(map.errors.iter().cloned()).collect()
}

/// `qrec keys` for people: per group, each binding's keys, label, id and
/// CLI form, then the problems.
pub(crate) fn keys(map: &Keymap) -> Vec<String> {
    let mut groups: Vec<&str> = Vec::new();
    for b in &map.bindings {
        if !groups.contains(&b.group) {
            groups.push(b.group);
        }
    }
    let mut lines = Vec::new();
    for group in groups {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(group.to_owned());
        let rows: Vec<Vec<String>> = map
            .bindings
            .iter()
            .filter(|b| b.group == group)
            .map(|b| {
                let keys: Vec<String> = b.keys.iter().map(chord).collect();
                vec![
                    if keys.is_empty() {
                        "(unbound)".to_owned()
                    } else {
                        keys.join(" ")
                    },
                    b.label.to_owned(),
                    b.id.to_owned(),
                    b.cli.unwrap_or("").to_owned(),
                ]
            })
            .collect();
        lines.extend(
            layout(&[], &rows, &[])
                .into_iter()
                .map(|l| format!("  {l}")),
        );
    }
    let problems = key_problems(map);
    if !problems.is_empty() {
        lines.push(String::new());
        lines.push("Problems".to_owned());
        lines.extend(problems.into_iter().map(|p| format!("  {p}")));
    }
    lines
}

/// The size and length of a finished export, when the message lacks them.
pub(crate) fn export_done(path: &str, bytes: u64, frame_count: u64) -> String {
    format!("{path} ({}, {})", human_bytes(bytes), frames(frame_count))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use qrec_core::keymap::{Binding, Conflict, KeyContext};
    use qrec_core::protocol::{AutocutClipView, AutocutSkipView};
    use qrec_core::state::{ClipRef, HostKind};
    use qrec_core::{JobId, TakeId, TakeStatus};
    use serde_json::json;

    use super::*;

    #[test]
    fn many_camera_modes_are_summarized() {
        let mode = |w: u64, h: u64, num: u64, den: u64, format: &str| json!({"width": w, "height": h, "fps": {"num": num, "den": den}, "format": format});
        let mut modes = vec![];
        for (w, h) in [(160, 90), (640, 480), (1280, 720), (1920, 1080)] {
            for fps in [5, 15, 30] {
                modes.push(mode(w, h, fps, 1, "YUY2"));
                modes.push(mode(w, h, fps, 1, "image/jpeg"));
            }
        }
        modes.push(mode(1280, 720, 60, 1, "image/jpeg"));
        modes.push(mode(2560, 1440, 2, 1, "YUY2"));
        modes.push(mode(2560, 1440, 2, 1, "YUY2")); // duplicate
        assert_eq!(
            camera_modes_summary(&modes),
            "2560x1440@2 YUY2, 1920x1080@30 YUY2, 1920x1080@30 MJPEG, 1280x720@60 MJPEG, +22 more"
        );
        assert_eq!(
            camera_modes_summary(&[mode(640, 480, 15, 2, "YUY2")]),
            "640x480@15/2 YUY2"
        );
        assert_eq!(camera_modes_summary(&[]), "");
    }

    fn history_row(seq: u64, label: &str, current: bool) -> HistoryRow {
        serde_json::from_value(json!({
            "seq": seq, "ts": "2026-10-14T12:00:00Z", "label": label, "current": current
        }))
        .unwrap()
    }

    #[test]
    fn status_lines() {
        let view: StatusView = serde_json::from_value(json!({
            "project": "swamp", "path": null, "initialized": true, "host": "cli", "pid": null,
            "record": {"phase": "recording", "take": "take-0003", "mode": "append",
                       "elapsedNs": 64_000_000_000u64, "waiters": []},
            "jobs": [], "takes": 3, "clips": 1, "durationFrames": 912, "cursor": 7
        }))
        .unwrap();
        let lines = status(&view, "/v/swamp", None);
        assert_eq!(lines[0], "Project    swamp");
        assert_eq!(lines[1], "Folder     /v/swamp");
        assert!(
            lines[2].starts_with("Host       none running; answered by this process (pid "),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "Recording  recording take-0003 (1 min 4.0 s)");
        assert_eq!(lines[4], "Jobs       none");
        assert_eq!(lines[5], "Timeline   1 clip, 912f");
        assert_eq!(lines[6], "Takes      3");
        assert_eq!(lines[7], "History    at seq 7");
        let hello = HelloInfo {
            pid: 9,
            project_dir: "/v/swamp".into(),
            project_dir_hex: None,
            host: HostKind::Gui,
            record: RecordPhase::Idle,
            jobs: vec![],
            version: "0.1.0".into(),
        };
        assert_eq!(
            status(&view, "/v/swamp", Some(&hello))[2],
            "Host       gui (pid 9)"
        );
        assert_eq!(status_empty("/v/new")[0], "Project  none yet");
    }

    #[test]
    fn record_phases() {
        assert_eq!(record_phase(&RecordPhase::Idle), "idle");
        let over = RecordMode::Over {
            clips: vec![ClipRef {
                id: "c1".parse().unwrap(),
                take: TakeId::numbered(1),
                in_: 0,
                out: 10,
            }],
        };
        let finalizing = RecordPhase::Finalizing {
            take: TakeId::numbered(2),
            mode: over.clone(),
            cancelled: true,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        assert_eq!(
            record_phase(&finalizing),
            "finalizing take-0002 (cancelled)"
        );
        let countdown: RecordPhase = serde_json::from_value(json!({
            "phase": "countdown", "remaining": 3, "mode": {"over": {"clips": [{"id": "c1", "take": "take-0001", "in": 0, "out": 10}]}},
            "timer": 4, "startReq": null, "wait": false
        }))
        .unwrap();
        assert_eq!(record_phase(&countdown), "countdown 3 s, over c1");
    }

    #[test]
    fn export_jobs_show_their_progress() {
        let export: Job = serde_json::from_value(json!({
            "id": 3, "req": null, "started": "2026-10-14T12:00:00Z",
            "kind": {"kind": "export", "out": "/v/out.mp4", "files": [], "totalFrames": 100,
                     "segFrames": [50, 50], "segDone": [50, 0], "stitchTotalNs": 0,
                     "stitchDoneNs": 0, "stitching": false}
        }))
        .unwrap();
        assert_eq!(job(&export), "export to /v/out.mp4 (45 %)");
        let purge = Job {
            id: JobId(1),
            kind: JobKind::Purge {
                takes: vec![TakeId::numbered(3)],
            },
            req: None,
            started: export.started,
        };
        assert_eq!(job(&purge), "purge of take-0003");
    }

    #[test]
    fn tables() {
        let takes: Vec<TakeRow> = serde_json::from_value(json!([
            {"id": "take-0001", "status": "ok", "recordedAt": "2026-10-14T12:03:10Z",
             "durationFrames": 912, "clips": 2, "unused": false, "error": null},
            {"id": "take-0002", "status": "broken", "recordedAt": "2026-10-14T12:05:00Z",
             "durationFrames": 0, "clips": 0, "unused": true, "error": "camera vanished"}
        ]))
        .unwrap();
        assert_eq!(
            take_list(&takes),
            [
                "TAKE       STATUS  LENGTH  CLIPS  RECORDED              NOTE",
                "take-0001  ok        912f      2  2026-10-14 12:03:10Z",
                "take-0002  broken      0f      0  2026-10-14 12:05:00Z  in the bin: camera vanished",
            ]
        );
        assert_eq!(take_list(&[]), ["No takes."]);
        assert_eq!(takes[1].status, TakeStatus::Broken);

        let clips: Vec<ClipRow> = serde_json::from_value(json!([
            {"id": "c7f3", "take": "take-0001", "in": 0, "out": 410, "start": 0, "duration": 410},
            {"id": "c8a1", "take": "take-0001", "in": 455, "out": 912, "start": 410, "duration": 457}
        ]))
        .unwrap();
        assert_eq!(
            clip_list(&clips),
            [
                "#  CLIP  TAKE         IN   OUT  START  LENGTH",
                "0  c7f3  take-0001    0f  410f     0f    410f",
                "1  c8a1  take-0001  455f  912f   410f    457f",
                "2 clips, 867f",
            ]
        );
        assert_eq!(clip_list(&[]), ["The timeline is empty."]);
        // Lengths a host reports are summed without overflowing.
        let huge: Vec<ClipRow> = serde_json::from_value(json!([
            {"id": "c1", "take": "take-0001", "in": 0, "out": u64::MAX, "start": 0, "duration": u64::MAX},
            {"id": "c2", "take": "take-0001", "in": 0, "out": u64::MAX, "start": u64::MAX, "duration": u64::MAX}
        ]))
        .unwrap();
        assert_eq!(
            clip_list(&huge).last().unwrap(),
            &format!("2 clips, {}f", u64::MAX)
        );

        let rows = [
            history_row(1, "Add layer L1", false),
            history_row(2, "Split clip c7f3", true),
            history_row(3, "Remove clip c8a1", false),
        ];
        let lines = history(&rows);
        assert_eq!(lines[0], "   SEQ  WHEN                  CHANGE");
        assert_eq!(lines[2], "*    2  2026-10-14 12:00:00Z  Split clip c7f3");
        assert_eq!(
            lines[4],
            "* = the current state; the entries after it can be redone"
        );
        let undone = [history_row(1, "Add layer L1", false)];
        assert_eq!(
            history(&undone)[2],
            "Everything listed is undone; `qrec redo` brings it back"
        );
        assert_eq!(history(&[]), ["No history yet."]);
    }

    #[test]
    fn autocut_plans() {
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: "c7f3".parse().unwrap(),
                kept: vec![[0, 120], [180, 400]],
                removed_frames: 60,
                silences: 1,
            }],
            skipped: vec![AutocutSkipView {
                clip: "c8a1".parse().unwrap(),
                reason: "entirely silent at -35 dB".into(),
            }],
        };
        assert_eq!(
            autocut_plan(&plan, true),
            [
                "CLIP  SILENCES  REMOVED  KEPT (take frames)",
                "c7f3         1      60f  0-120 180-400",
                "skipped c8a1: entirely silent at -35 dB",
                "Would remove 1 silence (60f). Run without --dry-run to cut.",
            ]
        );
        assert_eq!(autocut_plan(&plan, false).len(), 3);
        let empty = AutocutPlanView {
            clips: vec![],
            skipped: vec![],
        };
        assert_eq!(autocut_plan(&empty, true), ["Nothing to cut."]);
    }

    #[test]
    fn take_details() {
        let take = json!({
            "id": "take-0001", "status": "ok", "recordedAt": "2026-10-14T12:03:10Z",
            "durationFrames": 60, "error": null, "placed": true, "replaced": [], "unused": false,
            "scene": {"layers": [{"id": "L1", "name": "Screen FAKE-1"}], "audio": [{"id": "A1", "label": "Fake mic"}]},
            "streams": [
                {"id": "L1", "kind": "video", "file": "takes/take-0001/L1-screen-FAKE-1.mkv",
                 "width": 320, "height": 180, "startOffsetNs": 0, "frames": 60, "fps": {"num": 30, "den": 1}},
                {"id": "A1", "kind": "audio", "file": "takes/take-0001/A1-mic.mka",
                 "sampleRate": 48000, "channels": 1, "startOffsetNs": 3_000_000}
            ],
            "clips": [{"id": "c001", "take": "take-0001", "in": 0, "out": 60, "start": 0, "duration": 60}]
        });
        assert_eq!(
            take_show(&take),
            [
                "take-0001  ok  60f  recorded 2026-10-14T12:03:10Z",
                "Clips: c001 [0, 60) at 0f",
                "Streams",
                "  L1  video  takes/take-0001/L1-screen-FAKE-1.mkv  320x180, 30 fps, 60 frames",
                "  A1  audio  takes/take-0001/A1-mic.mka            48000 Hz, 1 ch, offset 3.0 ms",
                "Scene: 1 layer (L1 Screen FAKE-1), 1 audio track (A1 Fake mic)",
            ]
        );
    }

    #[test]
    fn sources_and_doctor() {
        let inv = json!({
            "screens": [{"output": "FAKE-1", "description": "Fake screen 1", "x": 0, "y": 0,
                         "width": 1280, "height": 720, "scale": 1.0}],
            "cameras": [{"device": "/dev/fake0", "label": "Fake camera",
                         "modes": [{"width": 640, "height": 480, "fps": {"num": 30, "den": 1}, "format": "YUY2"}]}],
            "mics": [{"name": "alsa_input.fake", "description": "Fake mic", "isMonitor": false}],
            "monitors": [],
            "defaultMic": "alsa_input.fake",
            "defaultMonitor": null
        });
        assert_eq!(
            sources(&inv),
            [
                "Screens",
                "  OUTPUT  SIZE      POSITION  SCALE  DESCRIPTION",
                "  FAKE-1  1280x720  0,0       1      Fake screen 1",
                "Cameras",
                "  DEVICE      LABEL        MODES",
                "  /dev/fake0  Fake camera  640x480@30 YUY2",
                "Mics",
                "  NAME             DESCRIPTION  DEFAULT",
                "  alsa_input.fake  Fake mic     yes",
                "System audio",
                "  none found",
            ]
        );

        let report = json!({
            "checks": [
                {"id": "ffmpeg", "required": true, "ok": true, "detail": "8.1.2", "hint": null},
                {"id": "wl-copy", "required": false, "ok": false, "detail": "not found", "hint": "install wl-clipboard"}
            ],
            "niriSnippet": "binds {\n}\n",
            "va": null
        });
        let (lines, failed) = doctor(&report, 0);
        assert!(!failed);
        assert_eq!(lines[1], "ok                  ffmpeg   8.1.2");
        assert_eq!(lines[2], "missing (optional)  wl-copy  not found");
        assert!(
            lines.contains(&"  wl-copy: install wl-clipboard".to_owned()),
            "{lines:?}"
        );
        assert!(lines.contains(&"binds {".to_owned()), "{lines:?}");
        assert_eq!(lines.last().unwrap(), "Everything Qrec needs is here.");
        let missing = json!({"checks": [{"id": "ffmpeg", "required": true, "ok": false, "detail": "not found"}]});
        let (lines, failed) = doctor(&missing, 0);
        assert!(failed);
        assert!(lines[1].starts_with("MISSING"), "{lines:?}");
    }

    #[test]
    fn keymaps() {
        let ctrl_z = KeyChord {
            key: "z".into(),
            ctrl: true,
            alt: false,
            shift: true,
            super_: false,
        };
        let map = Keymap {
            bindings: vec![
                Binding {
                    id: "edit.redo",
                    group: "Edit",
                    label: "Redo",
                    keys: vec![ctrl_z.clone()],
                    cli: Some("qrec redo"),
                    contexts: vec![KeyContext::Global],
                },
                Binding {
                    id: "view.fit",
                    group: "View",
                    label: "Zoom to fit",
                    keys: vec![],
                    cli: None,
                    contexts: vec![KeyContext::Timeline],
                },
            ],
            conflicts: vec![Conflict {
                chord: ctrl_z,
                ids: vec!["edit.redo".into(), "x.y".into()],
                context: KeyContext::Global,
            }],
            errors: vec!["keys.toml line 2: unknown id foo".into()],
        };
        assert_eq!(
            keys_json(&map)[0],
            json!({"id": "edit.redo", "group": "Edit", "label": "Redo", "keys": ["ctrl+shift+z"],
                   "cli": "qrec redo", "contexts": ["global"]})
        );
        assert_eq!(
            keys(&map),
            [
                "Edit",
                "  ctrl+shift+z  Redo  edit.redo  qrec redo",
                "",
                "View",
                "  (unbound)  Zoom to fit  view.fit",
                "",
                "Problems",
                "  ctrl+shift+z is bound to edit.redo and x.y in global; neither runs until keys.toml changes one",
                "  keys.toml line 2: unknown id foo",
            ]
        );
    }
}
