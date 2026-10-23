//! The `qrec` binary. With no command it opens the window (`qrec_gui`,
//! docs/GUI.md G§3.2); everything else is the CLI over the imperative
//! shell ([`shell::Shell`]).
//!
//! Order matters (A§8.10): a GPU probe child does its one job and exits;
//! then the platform (session, desktop, distribution) is read once from
//! the environment the process started with; then the GPU environment
//! (`LIBVA_DRIVER_NAME`, …) and, for the window, its display (a
//! `GDK_BACKEND` naming no display here goes) are set while the process
//! still has a single thread; only then does anything start threads.

mod shell;

use std::ffi::OsString;
use std::process::ExitCode;

use qrec_shell::gpu::{self, GpuInputs};
use qrec_shell::platform;

fn main() -> ExitCode {
    gpu::run_probe_if_requested();
    // Detected now, before anything changes the environment; the host and
    // GUI logs say it (`platform::current()` returns this from now on).
    platform::current();
    let args: Vec<OsString> = std::env::args_os().collect();
    // `qrec` without a command (global flags allowed) opens the GUI, which
    // also pins EGL and Vulkan. Parsing starts no thread.
    let is_gui = qrec_cli::parse_args(args.iter().cloned()).is_ok_and(|cli| cli.command.is_none());
    let inputs = GpuInputs::discover(&args, is_gui);
    gpu::prepare_gpu_env(&inputs);
    if is_gui {
        qrec_gui::prepare_display();
    }
    let mut backend = shell::Shell::new(&args, inputs);
    qrec_cli::run_process(&mut backend)
}
