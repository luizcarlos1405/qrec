//! Toast buttons that act on the desktop (G§3.7, `UiAction`): open a file,
//! show it in its folder, copy text or an image, or run a command. A
//! failure toasts ("Couldn't open <path>").
//!
//! `CopyImage` runs `wl-copy --type image/png` with the PNG on stdin through
//! the shell's process runner on `qrec-gui-io` (A§8.7: a timeout, stderr to
//! a log under `.qrec/logs/`, no unread pipe); toasts hide that action when
//! doctor's `wl-copy` check failed ([`copy_image_available`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio};
use qrec_core::protocol::{ToastLevel, UiAction};
use qrec_core::view::notices::Notice;
use qrec_shell::doctor::DoctorReport;
use qrec_shell::paths::ProjectPaths;
use qrec_shell::proc::{self, Limit, Spec};

use crate::{Ctx, guarded};

/// How long `wl-copy` may take (R6).
pub const WL_COPY_TIMEOUT: Duration = Duration::from_secs(5);

/// A path a core action names, for the desktop: project-relative paths
/// (`exports/…`, the export-done toast's, A§1.3) are under `project_dir`,
/// not under the window's working directory.
pub fn resolve(project_dir: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_dir.join(path)
    }
}

/// Carries out a toast button's `action` for the window `parent`.
pub fn run(ctx: &Ctx, parent: Option<&gtk::Window>, project_dir: &Path, action: &UiAction) {
    match action {
        UiAction::OpenPath(path) => {
            let file = gio::File::for_path(resolve(project_dir, path));
            let launcher = gtk::FileLauncher::new(Some(&file));
            let store = ctx.store.clone();
            let shown = path.clone();
            launcher.launch(
                parent,
                gio::Cancellable::NONE,
                guarded!("desktop: open", move |result| {
                    if let Err(e) = result {
                        store.notify(couldnt("open", &shown, &e.to_string()));
                    }
                }),
            );
        }
        UiAction::RevealPath(path) => {
            let file = gio::File::for_path(resolve(project_dir, path));
            let launcher = gtk::FileLauncher::new(Some(&file));
            let store = ctx.store.clone();
            let shown = path.clone();
            launcher.open_containing_folder(
                parent,
                gio::Cancellable::NONE,
                guarded!("desktop: reveal", move |result| {
                    if let Err(e) = result {
                        store.notify(couldnt("show", &shown, &e.to_string()));
                    }
                }),
            );
        }
        UiAction::CopyText(text) => match gdk::Display::default() {
            Some(display) => display.clipboard().set_text(text),
            None => ctx.store.notify(Notice::new(
                ToastLevel::Warning,
                "Couldn't copy: no display",
            )),
        },
        UiAction::CopyImage(path) => {
            let store = ctx.store.clone();
            let file = PathBuf::from(path);
            let paths = ProjectPaths::new(project_dir);
            ctx.media.run_io_then(
                move || copy_image(&paths, &file),
                move |result| {
                    if let Err(e) = result {
                        store.notify(
                            Notice::new(ToastLevel::Warning, "Couldn't copy the image")
                                .with_body(e),
                        );
                    }
                },
            );
        }
        UiAction::Dispatch(cmd) => ctx.store.dispatch((**cmd).clone()),
    }
}

/// Whether `CopyImage` can work: doctor found `wl-copy` (unknown before the
/// first check counts as yes).
pub fn copy_image_available(doctor: Option<&DoctorReport>) -> bool {
    doctor.is_none_or(|report| {
        report
            .checks
            .iter()
            .find(|c| c.id == "wl-copy")
            .is_none_or(|c| c.ok)
    })
}

/// `wl-copy --type image/png < file`, logged, bounded (on a worker).
fn copy_image(paths: &ProjectPaths, file: &Path) -> Result<(), String> {
    let (log, log_path) =
        proc::job_log(paths, None, "wl-copy").map_err(|e| format!("can't create a log: {e}"))?;
    let finished = Spec::new("wl-copy", Limit::Timeout(WL_COPY_TIMEOUT))
        .args(["--type", "image/png"])
        .stdin_file(file)
        .stderr_to(log)
        .run()
        .map_err(|e| e.to_string())?;
    if finished.success() {
        Ok(())
    } else {
        Err(format!(
            "{} (log: {})",
            finished.describe(),
            log_path.display()
        ))
    }
}

fn couldnt(verb: &str, path: &str, why: &str) -> Notice {
    Notice::new(ToastLevel::Warning, format!("Couldn't {verb} {path}")).with_body(why.to_owned())
}

#[cfg(test)]
mod tests {
    use qrec_shell::doctor::Check;

    use super::*;

    fn report(wl_copy: Option<bool>) -> DoctorReport {
        DoctorReport {
            checks: wl_copy
                .map(|ok| Check {
                    id: "wl-copy".into(),
                    required: false,
                    ok,
                    detail: String::new(),
                    hint: None,
                })
                .into_iter()
                .collect(),
            niri_snippet: String::new(),
            va: None,
        }
    }

    #[test]
    fn copy_image_hides_without_wl_copy() {
        assert!(copy_image_available(None));
        assert!(copy_image_available(Some(&report(None))));
        assert!(copy_image_available(Some(&report(Some(true)))));
        assert!(!copy_image_available(Some(&report(Some(false)))));
    }

    /// The export-done toast's paths are project-relative (A§1.3): they
    /// open under the project, wherever the window was started.
    #[test]
    fn relative_paths_are_the_projects() {
        let dir = Path::new("/home/me/talk");
        assert_eq!(
            resolve(dir, "exports/talk-20261014-153012.mp4"),
            Path::new("/home/me/talk/exports/talk-20261014-153012.mp4")
        );
        assert_eq!(
            resolve(dir, "/home/me/Videos/x.mp4"),
            Path::new("/home/me/Videos/x.mp4")
        );
    }

    #[test]
    fn failures_name_the_path() {
        let notice = couldnt("open", "/tmp/a.mp4", "no handler");
        assert_eq!(notice.title, "Couldn't open /tmp/a.mp4");
        assert_eq!(notice.body.as_deref(), Some("no handler"));
    }
}
