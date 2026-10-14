//! The `qrec` command line (SPEC §14, A§10).
//!
//! [`args`] is the clap command tree. [`run`] parses the process arguments
//! and, for now, answers help, version and completions itself; every other
//! command reports that it isn't implemented yet. The real dispatch (clap
//! args → `Request` → `Session`) replaces that fallback in `execute`.
//!
//! Exit codes follow SPEC §14: clap errors are usage errors (exit 2), and
//! under `--json` they print `{"ok":false,"error":{"code":"usage",…}}`.

#![forbid(unsafe_code)]

pub mod args;
mod output;
mod values;

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use qrec_core::{CoreError, ErrorCode};

pub use args::*;

/// Printed to stderr by `qrec` without a command until the GUI exists.
pub const GUI_NOT_BUILT: &str = "The Qrec GUI is not built yet: the screen designs are waiting for \
                                 product-owner approval (SPEC §15.1). Use the CLI: qrec --help";

/// Parses a full command line (`args[0]` is the program name).
///
/// Besides clap's own checks, it rejects a command's options combined with
/// one of its subcommands (`qrec export --format webm cancel`).
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Cli, clap::Error> {
    parse(args.into_iter().collect()).map(|(cli, _)| cli)
}

/// Runs `qrec` with the process arguments and standard streams.
pub fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let json = wants_json(&args);
    let code = match parse(args.clone()) {
        Ok((cli, path)) => execute(
            &cli,
            &path,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ),
        Err(err) if json && err.use_stderr() => usage_json(&err, &args, &mut io::stdout().lock()),
        Err(err) => {
            // Help and version go to stdout, errors to stderr, colored like clap does.
            let _ = err.print();
            clap_exit_code(&err)
        }
    };
    ExitCode::from(code)
}

/// [`run`] with explicit arguments and streams; returns the exit code.
/// Help and errors are written without colors.
pub fn run_with<I: IntoIterator<Item = OsString>>(
    args: I,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    let json = wants_json(&args);
    match parse(args.clone()) {
        Ok((cli, path)) => execute(&cli, &path, out, err),
        Err(e) if json && e.use_stderr() => usage_json(&e, &args, out),
        Err(e) => {
            let text = e.render().to_string();
            let _ = if e.use_stderr() {
                err.write_all(text.as_bytes())
            } else {
                out.write_all(text.as_bytes())
            };
            clap_exit_code(&e)
        }
    }
}

/// Parses `args` and returns the subcommand path (`["clip", "split"]`).
fn parse(args: Vec<OsString>) -> Result<(Cli, Vec<String>), clap::Error> {
    let mut command = Cli::command();
    let mut matches = command.try_get_matches_from_mut(args)?;
    check_subcommand_conflicts(&mut command, &matches)?;
    let mut path = Vec::new();
    let mut level = &matches;
    while let Some((name, sub)) = level.subcommand() {
        path.push(name.to_owned());
        level = sub;
    }
    let cli = Cli::from_arg_matches_mut(&mut matches).map_err(|e| e.format(&mut command))?;
    Ok((cli, path))
}

/// `autocut`, `history` and `export` take options and also have
/// subcommands; their own (non-global) options can't be combined with a
/// subcommand. clap's `args_conflicts_with_subcommands` would also reject
/// global flags placed before the subcommand (`qrec history --yes clear`).
fn check_subcommand_conflicts(
    command: &mut clap::Command,
    matches: &ArgMatches,
) -> Result<(), clap::Error> {
    let Some((name, sub_matches)) = matches.subcommand() else {
        return Ok(());
    };
    let given: Vec<String> = command
        .get_arguments()
        .filter(|arg| !arg.is_global_set() && !arg.is_positional())
        .filter(|arg| matches.value_source(arg.get_id().as_str()) == Some(ValueSource::CommandLine))
        .map(|arg| format!("--{}", arg.get_long().unwrap_or(arg.get_id().as_str())))
        .collect();
    if !given.is_empty() {
        let message = format!(
            "'{}' can't be combined with '{name}'; drop it or run '{} {name}' alone",
            given.join(", "),
            command.get_name()
        );
        return Err(command.error(clap::error::ErrorKind::ArgumentConflict, message));
    }
    match command.find_subcommand_mut(name) {
        Some(sub) => check_subcommand_conflicts(sub, sub_matches),
        None => Ok(()),
    }
}

/// Whether `--json` appears before a `--` terminator (used when clap fails).
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| *a == "--json")
}

fn clap_exit_code(err: &clap::Error) -> u8 {
    if err.use_stderr() {
        ErrorCode::Usage.exit_code()
    } else {
        0
    }
}

/// A clap error under `--json`: exactly `{"ok":false,"error":{"code":"usage","message":…}}`.
fn usage_json(err: &clap::Error, args: &[OsString], out: &mut dyn Write) -> u8 {
    let message = if output::is_help_on_missing(err) {
        missing_subcommand_message(args)
    } else {
        output::clap_error_message(err)
    };
    let _ = output::write_err(out, &CoreError::usage(message));
    ErrorCode::Usage.exit_code()
}

/// "'qrec scene' requires a subcommand: layer, audio, apply" for a bare group.
fn missing_subcommand_message(args: &[OsString]) -> String {
    let root = Cli::command();
    let mut command = &root;
    let mut path = vec![root.get_name().to_owned()];
    for arg in args.iter().skip(1).take_while(|a| *a != "--") {
        if let Some(sub) = arg.to_str().and_then(|a| command.find_subcommand(a)) {
            path.push(sub.get_name().to_owned());
            command = sub;
        }
    }
    let names: Vec<&str> = command
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| c.get_name())
        .collect();
    format!(
        "'{}' requires a subcommand: {}",
        path.join(" "),
        names.join(", ")
    )
}

/// Runs a parsed command line; returns the exit code.
fn execute(cli: &Cli, path: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let json = cli.global.json;
    match &cli.command {
        None => fail(CoreError::unavailable(GUI_NOT_BUILT), json, out, err),
        Some(Cmd::Version) => {
            let version = env!("CARGO_PKG_VERSION");
            let _ = if json {
                output::write_ok(out, &serde_json::json!({ "version": version }))
            } else {
                writeln!(out, "qrec {version}")
            };
            0
        }
        Some(Cmd::Completions(args)) => {
            let mut script = Vec::new();
            clap_complete::generate(args.shell, &mut Cli::command(), "qrec", &mut script);
            let _ = if json {
                let script = String::from_utf8_lossy(&script);
                output::write_ok(
                    out,
                    &serde_json::json!({ "shell": args.shell.to_string(), "script": script }),
                )
            } else {
                out.write_all(&script)
            };
            0
        }
        Some(_) => {
            let message = format!("qrec {}: not implemented yet", path.join(" "));
            fail(CoreError::unavailable(message), json, out, err)
        }
    }
}

/// Reports `error`: always as a line on stderr, plus the JSON envelope on
/// stdout under `--json`.
fn fail(error: CoreError, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let _ = writeln!(err, "{}", error.message);
    if json {
        let _ = output::write_err(out, &error);
    }
    error.exit_code()
}
