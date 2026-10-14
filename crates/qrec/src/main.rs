//! The `qrec` binary. With no command it will open the GUI (blocked until
//! the designs are approved, SPEC §15.1); everything else is the CLI over
//! the imperative shell ([`shell::Shell`]).
//!
//! Order matters (A§8.10): a GPU probe child does its one job and exits;
//! then the GPU environment (`LIBVA_DRIVER_NAME`, …) is set while the
//! process still has a single thread; only then does anything start
//! threads.

mod shell;

use std::ffi::OsString;
use std::process::ExitCode;

use qrec_shell::gpu::{self, GpuInputs};

fn main() -> ExitCode {
    gpu::run_probe_if_requested();
    let args: Vec<OsString> = std::env::args_os().collect();
    let is_gui = args.len() == 1;
    let inputs = GpuInputs::discover(&args, is_gui);
    gpu::prepare_gpu_env(&inputs);
    let mut backend = shell::Shell::new(&args, inputs);
    qrec_cli::run_process(&mut backend)
}
