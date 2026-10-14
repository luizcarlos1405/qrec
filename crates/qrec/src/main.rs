//! The `qrec` binary. With no command it will open the GUI (blocked until
//! the designs are approved, SPEC §15.1); everything else is the CLI.

use std::process::ExitCode;

fn main() -> ExitCode {
    qrec_cli::run()
}
