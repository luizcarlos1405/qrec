//! Toast buttons that act on the desktop (G§3.7, `UiAction`): open a file,
//! show it in its folder, copy text or an image, or run a command. A
//! failure toasts ("Couldn't open <path>").
//!
//! `CopyImage` reads and decodes the PNG on `qrec-gui-io` (no file work on
//! the GTK thread), then puts it on the display's clipboard
//! ([`image_provider`]): the file's exact bytes as `image/png`, its picture
//! (GTK converts it for the other image types a paste may ask for) and its
//! `file://` URI. The same on every GDK backend (Wayland, XWayland, Xorg),
//! with no helper and no window that takes the focus. Qrec serves it while
//! it runs; at quit, [`keep_copied_image`] hands an image still on the
//! clipboard on so that it outlives the window ([`Handoff`]).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use qrec_core::protocol::{ToastLevel, UiAction};
use qrec_core::view::notices::Notice;
use qrec_shell::paths::ProjectPaths;
use qrec_shell::proc::{self, Limit, Spec, WL_COPY_TIMEOUT};

use crate::media::MediaHub;
use crate::{Ctx, guard, guarded};

/// The longest the quit waits for an X11 clipboard manager to take the
/// copied image ([`Handoff::Store`]).
pub const STORE_WAIT: Duration = Duration::from_secs(1);

/// The first bytes of every PNG file.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

thread_local! {
    /// The image Copy image last put on the clipboard (main thread), for the
    /// quit. Weak: the clipboard lets go of it when another copy replaces it.
    static COPIED: RefCell<Option<Copied>> = const { RefCell::new(None) };
}

/// What [`keep_copied_image`] needs of the last Copy image.
struct Copied {
    provider: glib::WeakRef<gdk::ContentProvider>,
    /// The PNG's bytes as copied: what the quit hands on, whatever became
    /// of the file since (moved, renamed).
    png: glib::Bytes,
    file: PathBuf,
    paths: ProjectPaths,
}

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

/// The file a path action names, for the desktop ([`resolve`]d under
/// `project_dir`): Open file, Open folder, Copy image; `None` for the
/// others.
pub fn action_path(project_dir: &Path, action: &UiAction) -> Option<PathBuf> {
    match action {
        UiAction::OpenPath(path) | UiAction::RevealPath(path) | UiAction::CopyImage(path) => {
            Some(resolve(project_dir, path))
        }
        UiAction::CopyText(_) | UiAction::Dispatch(_) => None,
    }
}

/// Carries out a toast button's `action` for the window `parent`.
pub fn run(ctx: &Ctx, parent: Option<&gtk::Window>, project_dir: &Path, action: &UiAction) {
    let file_of = || action_path(project_dir, action).unwrap_or_default();
    match action {
        UiAction::OpenPath(path) => {
            let file = gio::File::for_path(file_of());
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
            let file = gio::File::for_path(file_of());
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
        UiAction::CopyImage(_) => {
            let store = ctx.store.clone();
            let file = file_of();
            let paths = ProjectPaths::new(project_dir);
            let read = file.clone();
            ctx.media.run_io_then(
                move || read_image(&read),
                move |image| {
                    if let Err(e) = image.and_then(|image| copy_image(&image, file, paths)) {
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

/// A PNG file, read and decoded on `qrec-gui-io` (both halves are `Send`).
pub struct Image {
    /// The file's bytes, exactly.
    pub png: glib::Bytes,
    /// Its picture.
    pub texture: gdk::Texture,
}

/// Reads and decodes the PNG `file` (on a worker: file work and decoding).
pub fn read_image(file: &Path) -> Result<Image, String> {
    let read = std::fs::read(file); // R1: qrec-gui-io
    let bytes = read.map_err(|e| format!("can't read {}: {e}", file.display()))?;
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(format!("{} isn't a PNG", file.display()));
    }
    let png = glib::Bytes::from_owned(bytes);
    let texture = gdk::Texture::from_bytes(&png)
        .map_err(|e| format!("can't decode {}: {e}", file.display()))?;
    Ok(Image { png, texture })
}

/// What Copy image offers, in this order: the PNG's exact bytes as
/// `image/png` (never encoded again), its texture (GTK serializes it for
/// the other image types, TIFF, JPEG, …, and hands it to a paste in Qrec)
/// and `uri_list` as `text/uri-list` (file managers, chat apps). A paste
/// asking for a type gets it from the first that has it.
pub fn image_provider(image: &Image, uri_list: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_bytes("image/png", &image.png),
        gdk::ContentProvider::for_value(&image.texture.to_value()),
        gdk::ContentProvider::for_bytes(
            "text/uri-list",
            &glib::Bytes::from_owned(uri_list.as_bytes().to_vec()),
        ),
    ])
}

/// The `text/uri-list` naming `file` (RFC 2483: one URI, CRLF-terminated).
pub fn uri_list(file: &Path) -> String {
    format!("{}\r\n", gio::File::for_path(file).uri())
}

/// Puts `image` (read from `file`) on the clipboard and remembers it for
/// the quit (main thread).
fn copy_image(image: &Image, file: PathBuf, paths: ProjectPaths) -> Result<(), String> {
    let display = gdk::Display::default().ok_or("no display")?;
    let provider = image_provider(image, &uri_list(&file));
    display
        .clipboard()
        .set_content(Some(&provider))
        .map_err(|e| e.to_string())?;
    tracing::info!("copied the image {}", file.display());
    COPIED.with(|copied| {
        copied.replace(Some(Copied {
            provider: provider.downgrade(),
            png: image.png.clone(),
            file,
            paths,
        }))
    });
    Ok(())
}

/// Where an image Qrec still serves on the clipboard goes at quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// Wayland: a detached `wl-copy --type image/png` serves the PNG once
    /// Qrec is gone (nothing else keeps a selection there). Without
    /// `wl-copy` the image goes with Qrec.
    WlCopy,
    /// X11 (and any other backend): GDK asks the clipboard manager
    /// (`CLIPBOARD_MANAGER`) to store it, waiting at most [`STORE_WAIT`].
    /// Without a manager GDK refuses at once. Then, `or_wl_copy`, in a
    /// Wayland session (XWayland: `GDK_BACKEND=x11` with `WAYLAND_DISPLAY`
    /// set, where X11 rarely has a manager), `wl-copy` serves it as on
    /// Wayland (XWayland passes it on to X11 clients); else the image
    /// goes with Qrec.
    Store { or_wl_copy: bool },
}

impl Handoff {
    /// For the GDK display whose type is named `display_type`
    /// (`GdkWaylandDisplay`, `GdkX11Display`, …), in a session with a
    /// Wayland display (`WAYLAND_DISPLAY`) or not.
    pub fn for_display(display_type: &str, wayland_session: bool) -> Self {
        if display_type == "GdkWaylandDisplay" {
            Self::WlCopy
        } else {
            Self::Store {
                or_wl_copy: wayland_session,
            }
        }
    }
}

/// Whether the session has a Wayland display for `wl-copy`
/// (`WAYLAND_DISPLAY`), whichever backend GDK took.
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
}

/// At quit (main thread): when the clipboard still holds the image Copy
/// image put there, hands it on so that it outlives the window
/// ([`Handoff`]); the `wl-copy` runs on `qrec-gui-io`, through the shell's
/// process runner. The flag it returns is set once the quit may end: at
/// once when there is nothing to hand on, else when the hand-off is over
/// (each step is bounded).
pub fn keep_copied_image(media: &MediaHub) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(true));
    let Some(copied) = COPIED.with(|copied| copied.borrow_mut().take()) else {
        return done;
    };
    let Some(display) = gdk::Display::default() else {
        return done;
    };
    let clipboard = display.clipboard();
    let still_ours = clipboard.is_local()
        && copied
            .provider
            .upgrade()
            .is_some_and(|provider| clipboard.content().as_ref() == Some(&provider));
    if !still_ours {
        tracing::debug!("quit: the copied image is no longer on the clipboard");
        return done;
    }
    done.store(false, Ordering::Release);
    // Sets the flag when the hand-off ends, however it ends.
    let over = Done(Arc::clone(&done));
    match Handoff::for_display(display.type_().name(), wayland_session()) {
        Handoff::WlCopy => hand_to_wl_copy(media, copied, over),
        Handoff::Store { or_wl_copy } => {
            let media = media.clone();
            store(&clipboard, move |stored| match stored {
                Ok(()) => tracing::info!("quit: the clipboard manager keeps the image"),
                Err(e) if or_wl_copy => {
                    tracing::info!("quit: no clipboard manager kept the image ({e}); to wl-copy");
                    hand_to_wl_copy(&media, copied, over);
                }
                Err(e) => tracing::info!("quit: the copied image goes with Qrec: {e}"),
            });
        }
    }
    done
}

/// Sets its flag when dropped: a hand-off that panicked is over too.
struct Done(Arc<AtomicBool>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Asks the clipboard manager to store the clipboard (main thread), at
/// most [`STORE_WAIT`]: `then` gets the answer, or an error once the wait
/// runs out (the store is cancelled; GDK need not honour it, `then` runs
/// once either way).
fn store(clipboard: &gdk::Clipboard, then: impl FnOnce(Result<(), String>) + 'static) {
    let then = Rc::new(Cell::new(Some(then)));
    let cancel = gio::Cancellable::new();
    let answer = Rc::clone(&then);
    clipboard.store_async(
        glib::Priority::DEFAULT,
        Some(&cancel),
        guarded!("quit: the clipboard stored", move |result| {
            if let Some(then) = answer.take() {
                then(result.map_err(|e| e.to_string()));
            }
        }),
    );
    guard::timeout_local_once(STORE_WAIT, "quit: the clipboard's store", move || {
        if let Some(then) = then.take() {
            cancel.cancel();
            then(Err(format!(
                "no clipboard manager took it within {} s",
                STORE_WAIT.as_secs()
            )));
        }
    });
}

/// Runs [`run_wl_copy`] with the copied PNG on `qrec-gui-io`; `over` sets
/// the quit's flag when it ends (or when the job is dropped).
fn hand_to_wl_copy(media: &MediaHub, copied: Copied, over: Done) {
    let Copied {
        png, file, paths, ..
    } = copied;
    media.run_io(move || {
        let _over = over;
        match run_wl_copy(&paths, &png) {
            Ok(()) => {
                tracing::info!("quit: wl-copy keeps the copied image {}", file.display());
            }
            Err(e) => tracing::info!("quit: the copied image goes with Qrec: {e}"),
        }
    });
}

/// `wl-copy --type image/png`, its stdin the `png` Copy image put on the
/// clipboard (from memory: not the file, which may have moved), logged,
/// bounded (on a worker): its forked server keeps serving the PNG after
/// Qrec exits (only the direct child dies with its owner thread).
fn run_wl_copy(paths: &ProjectPaths, png: &[u8]) -> Result<(), String> {
    let (log, log_path) =
        proc::job_log(paths, None, "wl-copy").map_err(|e| format!("can't create a log: {e}"))?;
    let finished = Spec::new("wl-copy", Limit::Timeout(WL_COPY_TIMEOUT))
        .args(["--type", "image/png"])
        .stdin_bytes(png)
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
    use super::*;

    /// A 2×2 PNG (GDK's encoder).
    fn png() -> glib::Bytes {
        let pixels = glib::Bytes::from_owned(vec![0x80u8; 2 * 2 * 4]);
        gdk::MemoryTexture::new(2, 2, gdk::MemoryFormat::R8g8b8a8, &pixels, 8).save_to_png_bytes()
    }

    fn written(provider: &gdk::ContentProvider, mime: &str) -> Vec<u8> {
        let stream = gio::MemoryOutputStream::new_resizable();
        glib::MainContext::new()
            .block_on(provider.write_mime_type_future(mime, &stream, glib::Priority::DEFAULT))
            .unwrap_or_else(|e| panic!("{mime}: {e}"));
        stream.close(gio::Cancellable::NONE).expect("closed");
        stream.steal_as_bytes().to_vec()
    }

    /// Copy image offers the PNG itself first (a paste of `image/png` gets
    /// the file's bytes, not GTK's encoding of the picture), the picture
    /// (on a display GTK serializes it for the other image types; without
    /// one it registers no serializers) and the file's URI.
    #[test]
    fn copy_image_offers_the_png_its_picture_and_its_uri() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let file = dir.path().join("talk-take-0004-00012345.png");
        std::fs::write(&file, png()).expect("the PNG");
        let image = read_image(&file).expect("read");
        let provider = image_provider(&image, &uri_list(&file));
        let formats = provider.formats();
        assert_eq!(
            formats
                .mime_types()
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>(),
            ["image/png", "text/uri-list"]
        );
        assert!(formats.contains_type(gdk::Texture::static_type()));
        assert_eq!(
            written(&provider, "image/png"),
            std::fs::read(&file).expect("the file")
        );
        assert_eq!(
            written(&provider, "text/uri-list"),
            uri_list(&file).into_bytes()
        );
        let value = provider
            .value(gdk::Texture::static_type())
            .expect("the texture");
        let texture: gdk::Texture = value.get().expect("a texture");
        assert_eq!((texture.width(), texture.height()), (2, 2));
    }

    #[test]
    fn copy_image_reads_only_pngs() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let missing = dir.path().join("gone.png");
        let err = read_image(&missing).err().expect("no file");
        assert!(err.starts_with("can't read "), "{err}");
        let text = dir.path().join("notes.png");
        std::fs::write(&text, "not a picture").expect("the file");
        let err = read_image(&text).err().expect("not a PNG");
        assert_eq!(err, format!("{} isn't a PNG", text.display()));
    }

    /// The URI is escaped (a project folder with a space) and ends the
    /// list's one line with CRLF.
    #[test]
    fn the_uri_list_names_the_file() {
        assert_eq!(
            uri_list(Path::new("/home/me/my talk/frames/x.png")),
            "file:///home/me/my%20talk/frames/x.png\r\n"
        );
    }

    /// Wayland keeps no selection for a client that left: a detached
    /// `wl-copy` serves it. X11 (and the rest) store it with the
    /// clipboard manager; XWayland in a Wayland session, which rarely has
    /// one, falls back to `wl-copy` when none took it.
    #[test]
    fn the_quit_hands_the_image_on_by_backend() {
        for wayland_session in [true, false] {
            assert_eq!(
                Handoff::for_display("GdkWaylandDisplay", wayland_session),
                Handoff::WlCopy
            );
        }
        assert_eq!(
            Handoff::for_display("GdkX11Display", true),
            Handoff::Store { or_wl_copy: true }
        );
        assert_eq!(
            Handoff::for_display("GdkX11Display", false),
            Handoff::Store { or_wl_copy: false }
        );
        assert_eq!(
            Handoff::for_display("GdkBroadwayDisplay", false),
            Handoff::Store { or_wl_copy: false }
        );
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

    /// Every path action opens under the project folder, whatever the
    /// window's working directory: a project-relative PNG (`frames/…`,
    /// A§1.3) is copied from the project, as Open file opens an export.
    #[test]
    fn path_actions_resolve_under_the_project() {
        let dir = Path::new("/home/me/talk");
        let shot = "frames/talk-take-0004-00012345.png";
        for action in [
            UiAction::OpenPath(shot.into()),
            UiAction::RevealPath(shot.into()),
            UiAction::CopyImage(shot.into()),
        ] {
            assert_eq!(
                action_path(dir, &action).as_deref(),
                Some(Path::new(
                    "/home/me/talk/frames/talk-take-0004-00012345.png"
                )),
                "{action:?}"
            );
        }
        assert_eq!(
            action_path(dir, &UiAction::CopyImage("/tmp/x.png".into())).as_deref(),
            Some(Path::new("/tmp/x.png"))
        );
        assert_eq!(action_path(dir, &UiAction::CopyText(shot.into())), None);
    }

    #[test]
    fn failures_name_the_path() {
        let notice = couldnt("open", "/tmp/a.mp4", "no handler");
        assert_eq!(notice.title, "Couldn't open /tmp/a.mp4");
        assert_eq!(notice.body.as_deref(), Some("no handler"));
    }
}
