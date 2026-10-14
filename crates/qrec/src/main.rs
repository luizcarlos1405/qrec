//! The `qrec` binary. With no command it will open the GUI (blocked until
//! the designs are approved, SPEC §15.1); everything else is the CLI.
//!
//! The CLI reaches the imperative shell through the `qrec_cli::Backend`
//! port. This crate doesn't depend on `qrec-shell` yet, so it runs the CLI
//! over `qrec_cli::Standalone` (project-less commands only). Once it does,
//! `main` first calls `qrec_shell::gpu::prepare_gpu_env` (before any thread
//! starts, A§8.10) and then `qrec_cli::run_process` with the shell's backend.

use std::process::ExitCode;

fn main() -> ExitCode {
    qrec_cli::run()
}
