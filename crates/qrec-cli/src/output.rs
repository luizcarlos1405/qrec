//! The `--json` envelope (SPEC §14 output contract) and the one-line
//! messages for clap errors under `--json`.

use std::io::{self, Write};

use clap::error::ErrorKind;
use qrec_core::CoreError;
use serde::Serialize;

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

/// Writes `{"ok":true,"data":data}` and a newline.
pub(crate) fn write_ok<T: Serialize>(out: &mut dyn Write, data: &T) -> io::Result<()> {
    write_line(out, &WireOk { ok: true, data })
}

/// Writes `{"ok":false,"error":error}` and a newline.
pub(crate) fn write_err(out: &mut dyn Write, error: &CoreError) -> io::Result<()> {
    write_line(out, &WireErr { ok: false, error })
}

fn write_line<T: Serialize>(out: &mut dyn Write, value: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
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
