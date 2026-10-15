//! The `qrec` command line (SPEC §14, A§10).
//!
//! - [`args`]: the clap command tree.
//! - `plan`: a parsed command line → a request for the project's host
//!   (`Command`/`Query`, A§4.4) or a project-less action.
//! - `resolve`: the folder rule (SPEC §4.4, A§8.4) picks the project; a
//!   [`Backend`] routes the request to its host or runs it in-process
//!   (A§8.3–A§8.6). The binary implements it over `qrec-shell`;
//!   [`Standalone`] is the backend of a build without the shell.
//! - Replies are printed for people (short lines, aligned tables, a
//!   progress line on stderr) or, with `--json`, as exactly one
//!   `{"ok":…}` object, after `{"event":"progress",…}` lines for streaming
//!   commands and with notices as JSON lines on stderr (A§4.9).
//!
//! Exit codes follow SPEC §14 / A§4.8: clap errors are usage errors
//! (exit 2), core errors map through `CoreError::exit_code`, Ctrl+C is 130.

#![forbid(unsafe_code)]

pub mod args;
pub mod backend;
mod config;
mod exec;
mod human;
mod output;
mod plan;
mod resolve;
mod standalone;
mod table;
mod values;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use qrec_core::{CoreError, ErrorCode};

pub use args::*;
pub use backend::*;
pub use exec::{EXIT_INTERRUPTED, RunEnv};
pub use standalone::Standalone;

/// Printed to stderr by `qrec` without a command until the GUI exists.
pub const GUI_NOT_BUILT: &str = "The Qrec GUI is not built yet: the screen designs are waiting for \
                                 product-owner approval (SPEC §15.1). Use the CLI: qrec --help";

/// Parses a full command line (`args[0]` is the program name).
///
/// Besides clap's own checks, it rejects a command's options combined with
/// one of its subcommands (`qrec export --format webm cancel`).
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Cli, clap::Error> {
    parse(args.into_iter().collect())
}

/// Runs `qrec` with the process arguments, standard streams and current
/// directory, without the shell ([`Standalone`]): project-less commands
/// work, the others report that they're unavailable in this build.
pub fn run() -> ExitCode {
    run_process(&mut Standalone::from_env())
}

/// Runs `qrec` with the process arguments, standard streams and current
/// directory over `backend`. Help and clap errors are colored like clap
/// does; stdout and stderr are written unlocked, so the backend's threads
/// can log meanwhile.
pub fn run_process(backend: &mut dyn Backend) -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let env = RunEnv {
        cwd: std::env::current_dir().ok(),
        err_tty: io::stderr().is_terminal(),
    };
    let json = wants_json(&args);
    let code = match parse(args.clone()) {
        Ok(cli) => exec::execute(&cli, backend, &env, &mut io::stdout(), &mut io::stderr()),
        Err(err) if json => clap_json(&err, &args, &mut io::stdout(), &mut io::stderr()),
        Err(err) => {
            // Help and version go to stdout, errors to stderr.
            let printed = err.print();
            clap_exit_code(&err, printed, &mut io::stderr())
        }
    };
    ExitCode::from(code)
}

/// [`run_process`] with explicit arguments and streams and the process's
/// current directory, without the shell ([`Standalone::from_env`]).
/// Help and errors are written without colors.
pub fn run_with<I: IntoIterator<Item = OsString>>(
    args: I,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let env = RunEnv {
        cwd: std::env::current_dir().ok(),
        err_tty: false,
    };
    run_with_backend(args, &mut Standalone::from_env(), &env, out, err)
}

/// Runs a command line over `backend` in `env`, writing to `out` and
/// `err`; returns the exit code. Help and errors are written without colors.
pub fn run_with_backend<I: IntoIterator<Item = OsString>>(
    args: I,
    backend: &mut dyn Backend,
    env: &RunEnv,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    let json = wants_json(&args);
    match parse(args.clone()) {
        Ok(cli) => exec::execute(&cli, backend, env, out, err),
        Err(e) if json => clap_json(&e, &args, out, err),
        Err(e) => {
            let text = e.render().to_string();
            let printed = if e.use_stderr() {
                err.write_all(text.as_bytes())
            } else {
                out.write_all(text.as_bytes()).and_then(|()| out.flush())
            };
            clap_exit_code(&e, printed, err)
        }
    }
}

/// Parses `args` with the extra subcommand-conflict check.
fn parse(args: Vec<OsString>) -> Result<Cli, clap::Error> {
    let mut command = Cli::command();
    let mut matches = command.try_get_matches_from_mut(args)?;
    check_subcommand_conflicts(&mut command, &matches)?;
    Cli::from_arg_matches_mut(&mut matches).map_err(|e| e.format(&mut command))
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

/// The exit code after printing a clap error (2), or help or the version
/// (0). Help that stdout couldn't take for another reason than a closed
/// pipe (`qrec --help | head -1` is fine; a full disk isn't) fails the run
/// with exit 1, said on stderr (A§8.6).
fn clap_exit_code(err: &clap::Error, printed: io::Result<()>, stderr: &mut dyn Write) -> u8 {
    if err.use_stderr() {
        return ErrorCode::Usage.exit_code();
    }
    match printed {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            let notice = output::human_notice(&output::lost_output(&e));
            let _ = stderr.write_all(notice.as_bytes());
            ErrorCode::Io.exit_code()
        }
        _ => 0,
    }
}

/// What clap stopped at, under `--json`: exactly one object, like every
/// command (SPEC §14). Help and the version are answers,
/// `{"ok":true,"data":<help text>}` and `{"ok":true,"data":{"version":…}}`
/// (what `qrec version --json` prints); everything else is
/// `{"ok":false,"error":{"code":"usage","message":…}}` (exit 2).
fn clap_json(
    err: &clap::Error,
    args: &[OsString],
    out: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let mut ui = output::Ui::new(out, stderr, true, false, 0, false);
    let code = match err.kind() {
        clap::error::ErrorKind::DisplayVersion => {
            ui.print_ok(&exec::version_data());
            0
        }
        clap::error::ErrorKind::DisplayHelp => {
            // Rendered without colors.
            ui.print_ok(&serde_json::Value::String(err.render().to_string()));
            0
        }
        _ => {
            let message = if output::is_help_on_missing(err) {
                missing_subcommand_message(args)
            } else {
                output::clap_error_message(err)
            };
            ui.fail(&CoreError::usage(message))
        }
    };
    ui.finish(code)
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
