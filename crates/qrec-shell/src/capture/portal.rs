//! The ScreenCast portal for the real backend (A§8.11, SPEC §5.1).
//!
//! One portal session per screen layer, `persist_mode = 2`, restore tokens
//! in `<state>/portal-tokens.json` keyed by output name. The stream the
//! portal returns is matched to an output by position and size (when the
//! screens' geometry is known), so a token is always stored under the
//! monitor it really belongs to. Before the first portal call the process
//! registers the app id `com.rsensato.qrec` with the host Registry
//! (xdg-desktop-portal ≥ 1.19; older portals just don't have it).
//!
//! ashpd runs under `futures_lite::future::block_on` on a dedicated worker
//! thread, so the engine thread stays responsive while a picker dialog is
//! open; every request has a deadline. Never used by the fake backend.

use std::future::Future;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
    StartCastOptions, Stream,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, ResponseError, Session};
use crossbeam_channel::{Receiver, Sender};
use qrec_core::ctx::ScreenInfo;

use super::APP_ID;
use super::engine::Cmd;
use super::tokens::{PortalTokens, TokenEntry};

/// How long closing a session may take.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// An open screen-share: the PipeWire remote and node of one monitor.
pub(super) struct ScreenShare {
    pub output: String,
    pub node_id: u32,
    fd: OwnedFd,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    session: Option<Session<Screencast>>,
}

impl ScreenShare {
    /// The PipeWire remote fd for `pipewiresrc fd=` (which dups it).
    pub fn fd_raw(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

impl std::fmt::Debug for ScreenShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenShare")
            .field("output", &self.output)
            .field("node_id", &self.node_id)
            .field("position", &self.position)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A request to open casts for some outputs.
pub(super) struct OpenRequest {
    pub id: u64,
    pub outputs: Vec<String>,
    /// Known screens (for matching streams to outputs); may be empty.
    pub screens: Vec<ScreenInfo>,
    pub tokens: PathBuf,
    pub timeout: Duration,
    pub register_app_id: bool,
}

enum Job {
    Open(OpenRequest, Sender<Cmd>),
    Close(Vec<ScreenShare>),
}

/// The portal thread.
pub(super) struct PortalWorker {
    tx: Sender<Job>,
}

impl PortalWorker {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        thread::Builder::new()
            .name("qrec-portal".into())
            .spawn(move || worker(rx))?;
        Ok(Self { tx })
    }

    /// Opens casts; the result comes back as `Cmd::PortalDone`.
    pub fn open(&self, request: OpenRequest, reply: Sender<Cmd>) -> bool {
        self.tx.send(Job::Open(request, reply)).is_ok()
    }

    /// Closes casts' portal sessions (best effort, bounded).
    pub fn close(&self, casts: Vec<ScreenShare>) {
        if !casts.is_empty() {
            let _ = self.tx.send(Job::Close(casts));
        }
    }
}

fn worker(rx: Receiver<Job>) {
    let mut registered = false;
    for job in rx {
        match job {
            Job::Open(request, reply) => {
                if request.register_app_id && !registered {
                    registered = true;
                    register_app_id();
                }
                let results = open_all(&request);
                let _ = reply.send(Cmd::PortalDone {
                    id: request.id,
                    results,
                });
            }
            Job::Close(casts) => {
                for cast in casts {
                    if let Some(session) = cast.session {
                        let _ =
                            run_with_deadline(CLOSE_TIMEOUT, async move { session.close().await });
                    }
                }
            }
        }
    }
}

/// Registers `com.rsensato.qrec` for this process with the host portal
/// Registry. Unsupported portals or a sandbox make this a no-op.
fn register_app_id() {
    let Ok(app_id) = APP_ID.parse::<ashpd::AppID>() else {
        return;
    };
    match run_with_deadline(CLOSE_TIMEOUT, ashpd::register_host_app(app_id)) {
        Some(Ok(())) => tracing::debug!("registered {APP_ID} with the portal"),
        Some(Err(e)) => tracing::info!("the portal Registry isn't available ({e}); continuing"),
        None => tracing::info!("the portal Registry didn't answer; continuing"),
    }
}

fn open_all(request: &OpenRequest) -> Vec<(String, Result<ScreenShare, String>)> {
    let mut tokens = PortalTokens::load(&request.tokens);
    let results = request
        .outputs
        .iter()
        .map(|output| {
            let result = open_one(output, request, &mut tokens);
            (output.clone(), result)
        })
        .collect();
    if let Err(e) = tokens.save(&request.tokens) {
        tracing::warn!("can't save {}: {e}", request.tokens.display());
    }
    results
}

/// A portal session waiting for its dialog.
struct Pending {
    proxy: Screencast,
    session: Session<Screencast>,
    cursor: CursorMode,
}

/// What a shared stream turned into.
struct Shared {
    node_id: u32,
    fd: OwnedFd,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

/// One screen: a session (bounded by [`CLOSE_TIMEOUT`]), then the dialog
/// (bounded by the request's timeout). A session whose dialog fails, times
/// out or shares the wrong screen is closed, which also takes a dialog
/// still showing off the screen.
fn open_one(
    output: &str,
    request: &OpenRequest,
    tokens: &mut PortalTokens,
) -> Result<ScreenShare, String> {
    let pending = run_with_deadline(CLOSE_TIMEOUT, begin()).unwrap_or_else(|| {
        Err("the ScreenCast portal didn't answer (is xdg-desktop-portal running?)".to_owned())
    })?;
    let shared = run_with_deadline(
        request.timeout,
        share(&pending, output, &request.screens, tokens),
    )
    .unwrap_or_else(|| {
        Err(format!(
            "the screen-share dialog for {output} wasn't answered within {} s",
            request.timeout.as_secs()
        ))
    });
    match shared {
        Ok(shared) => Ok(ScreenShare {
            output: output.to_owned(),
            node_id: shared.node_id,
            fd: shared.fd,
            position: shared.position,
            size: shared.size,
            session: Some(pending.session),
        }),
        Err(message) => {
            let session = pending.session;
            let _ = run_with_deadline(CLOSE_TIMEOUT, async move { session.close().await });
            Err(message)
        }
    }
}

async fn begin() -> Result<Pending, String> {
    let proxy = Screencast::new()
        .await
        .map_err(|e| describe("the ScreenCast portal isn't available", e))?;
    let cursor = match proxy.available_cursor_modes().await {
        Ok(modes) if modes.contains(CursorMode::Embedded) => CursorMode::Embedded,
        Ok(modes) if modes.contains(CursorMode::Hidden) => CursorMode::Hidden,
        _ => CursorMode::Embedded,
    };
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| describe("can't create a screen-share session", e))?;
    Ok(Pending {
        proxy,
        session,
        cursor,
    })
}

/// Shows the dialog (or restores `output`'s token), checks which screen was
/// shared, keeps the new token and opens the PipeWire remote.
async fn share(
    pending: &Pending,
    output: &str,
    screens: &[ScreenInfo],
    tokens: &mut PortalTokens,
) -> Result<Shared, String> {
    let Pending {
        proxy,
        session,
        cursor,
    } = pending;
    let token = tokens.get(output).map(|t| t.token.clone());
    let selected = async {
        proxy
            .select_sources(
                session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(*cursor)
                    .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token.as_deref()),
            )
            .await?
            .response()?;
        proxy
            .start(session, None, StartCastOptions::default())
            .await?
            .response()
    }
    .await;
    let streams =
        selected.map_err(|e| describe(&format!("screen sharing for {output} failed"), e))?;
    let stream = streams
        .streams()
        .first()
        .cloned()
        .ok_or_else(|| format!("the portal shared no stream for {output}"))?;
    let identified = identify(&stream, screens);
    if let Some(new_token) = streams.restore_token() {
        let key = identified.clone().unwrap_or_else(|| output.to_owned());
        tokens.set(
            &key,
            TokenEntry {
                token: new_token.to_owned(),
                position: stream.position(),
                size: stream.size(),
                updated: Some(
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
            },
        );
    } else if identified.as_deref().is_none_or(|o| o == output) {
        // The portal doesn't persist: drop the stale token.
        tokens.remove(output);
    }
    if let Some(other) = identified.filter(|o| o != output) {
        return Err(format!(
            "the screen-share dialog picked {other}, not {output}; pick {output} next time"
        ));
    }
    let fd = proxy
        .open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default())
        .await
        .map_err(|e| describe("can't open the PipeWire remote", e))?;
    Ok(Shared {
        node_id: stream.pipe_wire_node_id(),
        fd,
        position: stream.position(),
        size: stream.size(),
    })
}

/// The output a stream shows, by position and size, when the screens are known.
fn identify(stream: &Stream, screens: &[ScreenInfo]) -> Option<String> {
    let position = stream.position()?;
    screens
        .iter()
        .find(|s| position == (s.x, s.y) && size_matches(stream.size(), s))
        .map(|s| s.output.clone())
}

/// Portal sizes are in compositor (logical) coordinates; screens may report
/// pixels. Either matches.
fn size_matches(size: Option<(i32, i32)>, screen: &ScreenInfo) -> bool {
    let Some((w, h)) = size else { return true };
    let pixels = (screen.width as i32, screen.height as i32);
    let scale = if screen.scale > 0.0 {
        screen.scale
    } else {
        1.0
    };
    let logical = (
        (f64::from(screen.width) / scale).round() as i32,
        (f64::from(screen.height) / scale).round() as i32,
    );
    (w, h) == pixels || (w, h) == logical
}

fn describe(what: &str, e: ashpd::Error) -> String {
    match e {
        ashpd::Error::Response(ResponseError::Cancelled) => {
            format!("{what}: screen sharing was cancelled in the dialog")
        }
        ashpd::Error::PortalNotFound(_) => format!(
            "{what}: no ScreenCast portal (install xdg-desktop-portal with a ScreenCast backend)"
        ),
        other => format!("{what}: {other}"),
    }
}

/// Runs `future` on this thread until it finishes or `timeout` passes.
fn run_with_deadline<F: Future>(timeout: Duration, future: F) -> Option<F::Output> {
    let deadline = Deadline::new(timeout);
    futures_lite::future::block_on(futures_lite::future::or(
        async { Some(future.await) },
        async {
            deadline.await;
            None
        },
    ))
}

/// A future that completes at a deadline (woken by a helper thread).
struct Deadline {
    at: Instant,
    shared: Arc<DeadlineShared>,
    armed: bool,
}

struct DeadlineShared {
    fired: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Deadline {
    fn new(timeout: Duration) -> Self {
        Self {
            at: Instant::now() + timeout,
            shared: Arc::new(DeadlineShared {
                fired: AtomicBool::new(false),
                waker: Mutex::new(None),
            }),
            armed: false,
        }
    }
}

impl Future for Deadline {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.shared.fired.load(Ordering::SeqCst) || Instant::now() >= self.at {
            return Poll::Ready(());
        }
        *self
            .shared
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        if !self.armed {
            self.armed = true;
            let shared = self.shared.clone();
            let at = self.at;
            let spawned = thread::Builder::new()
                .name("qrec-portal-deadline".into())
                .spawn(move || {
                    let now = Instant::now();
                    if at > now {
                        thread::sleep(at - now);
                    }
                    shared.fired.store(true, Ordering::SeqCst);
                    if let Some(waker) = shared
                        .waker
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take()
                    {
                        waker.wake();
                    }
                });
            if spawned.is_err() {
                // No timer thread: give up waiting rather than risk forever.
                return Poll::Ready(());
            }
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(output: &str, x: i32, w: u32, h: u32, scale: f64) -> ScreenInfo {
        ScreenInfo {
            output: output.into(),
            description: String::new(),
            x,
            y: 0,
            width: w,
            height: h,
            scale,
        }
    }

    #[test]
    fn sizes_match_in_pixels_or_logical_units() {
        let s = screen("DP-4", 0, 2560, 1440, 1.25);
        assert!(size_matches(Some((2560, 1440)), &s));
        assert!(size_matches(Some((2048, 1152)), &s));
        assert!(!size_matches(Some((1920, 1080)), &s));
        assert!(size_matches(None, &s));
    }

    #[test]
    fn deadlines_bound_futures() {
        let started = Instant::now();
        let never = std::future::pending::<()>();
        assert_eq!(run_with_deadline(Duration::from_millis(50), never), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            run_with_deadline(Duration::from_secs(5), async { 7 }),
            Some(7)
        );
    }
}
