//! The ScreenCast portal for the real backend (A§8.11, SPEC §5.1).
//!
//! One portal session per screen layer, `persist_mode = 2`, restore tokens
//! in `<state>/portal-tokens.json` keyed by output name. The stream the
//! portal returns is matched to a discovered screen by its mapping id (the
//! output's name from KDE, COSMIC, xdg-desktop-portal-hyprland 1.4.0 and
//! later, and xdg-desktop-portal-wlr's main branch, not its 0.8 releases),
//! else by position and size (niri, GNOME, wlroots), else by a size no
//! other screen has (a stream without a position, or on Hyprland, whose
//! portal says 0,0 for every monitor), so a token is only ever stored
//! under the monitor it really belongs to. A stream nothing identifies is
//! still shared for that request, but its token isn't kept: the dialog
//! shows again next time. Before the first portal call the process
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
use crate::platform::{self, Desktop};

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
    /// Known screens (for matching streams to outputs); may be empty, and
    /// then no stream is identified, so no token is kept.
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
/// shared ([`identify`]), files the new token ([`file_token`]) and opens
/// the PipeWire remote. A stream nothing identifies is still shared for
/// this request, with a warning; only its token isn't kept.
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
    let facts = StreamFacts::of(&stream);
    let positions = Positions::of(&platform::current().desktop);
    let identified = identify(&facts, screens, positions);
    let entry = streams.restore_token().map(|token| TokenEntry {
        token: token.to_owned(),
        position: facts.position,
        size: facts.size,
        updated: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    });
    file_token(
        tokens,
        output,
        identified.as_ref().map(|found| found.output.as_str()),
        entry,
    );
    match identified {
        Some(found) if found.output != output => {
            return Err(format!(
                "the screen-share dialog picked {}, not {output}; pick {output} next time",
                found.output
            ));
        }
        Some(found) => tracing::debug!("capture: the portal shared {output} ({})", found.by),
        None => tracing::warn!(
            "capture: can't verify which monitor the portal shared for {output} ({facts}; \
             screens: {}): using it as {output} for now, without keeping its restore token, \
             so the screen-share dialog shows again next time{}",
            describe_screens(screens),
            positions.unverified_hint(&facts)
        ),
    }
    let fd = proxy
        .open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default())
        .await
        .map_err(|e| describe("can't open the PipeWire remote", e))?;
    Ok(Shared {
        node_id: stream.pipe_wire_node_id(),
        fd,
        position: facts.position,
        size: facts.size,
    })
}

/// What a portal stream says about the screen it shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StreamFacts {
    /// What the portal backend maps the stream to: the output's name from
    /// KDE, COSMIC, xdph 1.4.0 and later and xdpw's main branch (xdph
    /// before 1.4.0 and xdpw's 0.8 releases send none); an opaque UUID from
    /// GNOME, in remote-desktop sessions only; nothing from niri.
    mapping_id: Option<String>,
    /// In the compositor's (logical) space; xdph says 0,0 for every monitor
    /// ([`Positions::Placeholder`]).
    position: Option<(i32, i32)>,
    /// Logical, or the buffer's pixels (xdph, COSMIC). xdph's buffer is in
    /// the panel's own orientation: it leaves the output's transform to the
    /// consumer, so a rotated monitor's stream has its sides swapped.
    size: Option<(i32, i32)>,
    source_type: Option<SourceType>,
}

impl StreamFacts {
    fn of(stream: &Stream) -> Self {
        Self {
            mapping_id: stream.mapping_id().map(str::to_owned),
            position: stream.position(),
            size: stream.size(),
            source_type: stream.source_type(),
        }
    }
}

impl std::fmt::Display for StreamFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.mapping_id {
            Some(id) => write!(f, "mapping id {id:?}")?,
            None => f.write_str("no mapping id")?,
        }
        match self.position {
            Some((x, y)) => write!(f, ", position {x},{y}")?,
            None => f.write_str(", no position")?,
        }
        match self.size {
            Some((w, h)) => write!(f, ", size {w}x{h}")?,
            None => f.write_str(", no size")?,
        }
        match self.source_type {
            Some(SourceType::Monitor) => Ok(()),
            Some(other) => write!(f, ", a {other:?} stream"),
            None => f.write_str(", no source type"),
        }
    }
}

/// The screen a stream shows, and what says so.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identified {
    output: String,
    by: Evidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// The stream's mapping id is the screen's name.
    MappingId,
    /// Its position and size are the screen's, and no other screen's.
    Geometry,
    /// It has no position (or only a placeholder), and only this screen
    /// has its size, either way round.
    UniqueSize,
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Evidence::MappingId => "by its mapping id",
            Evidence::Geometry => "by its position and size",
            Evidence::UniqueSize => "by its size, which no other screen has",
        })
    }
}

/// What a stream's position says on this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Positions {
    /// Where its monitor is, in the compositor's space (niri, GNOME,
    /// wlroots).
    Real,
    /// Nothing: xdg-desktop-portal-hyprland says 0,0 for every monitor.
    /// From 1.4.0 it names the monitor in the mapping id; before, only a
    /// size no other screen has tells which monitor a stream shows.
    Placeholder,
}

impl Positions {
    /// Placeholders on Hyprland (its portal is xdph), real everywhere else.
    fn of(desktop: &Desktop) -> Self {
        match desktop {
            Desktop::Hyprland => Positions::Placeholder,
            _ => Positions::Real,
        }
    }

    /// What to add to the log line about a stream nothing identified: on
    /// Hyprland, without a mapping id, the portal's version.
    fn unverified_hint(self, facts: &StreamFacts) -> &'static str {
        match (self, facts.mapping_id.as_deref()) {
            (Positions::Placeholder, None | Some("")) => {
                " (on Hyprland, only xdg-desktop-portal-hyprland 1.4.0 and later say which \
                 monitor they share)"
            }
            _ => "",
        }
    }
}

/// The screen `stream` shows among `screens` (discovered for this request):
/// the one its mapping id names; else, when `positions` are real, the only
/// one with its position and size; else, when it has no position or only a
/// placeholder (xdph before 1.4.0), the only one with its size, either way
/// round (xdph's buffer is in the panel's own orientation).
///
/// A mapping id that names no known screen leaves the stream unidentified
/// (a monitor that came after the discovery, say): its position can't be
/// trusted then, as xdph says 0,0 for every monitor. Only a UUID (GNOME's)
/// is an id that never names a screen; the geometry decides as if there
/// were none. `None` too for a stream that isn't a monitor, a geometry no
/// screen or several screens have (mirrors, two monitors of one size under
/// placeholder positions), and when no screen is known.
fn identify(
    stream: &StreamFacts,
    screens: &[ScreenInfo],
    positions: Positions,
) -> Option<Identified> {
    if stream
        .source_type
        .is_some_and(|kind| kind != SourceType::Monitor)
    {
        return None;
    }
    let found = |screen: &ScreenInfo, by| {
        Some(Identified {
            output: screen.output.clone(),
            by,
        })
    };
    if let Some(id) = stream.mapping_id.as_deref().filter(|id| !id.is_empty()) {
        if let Some(screen) = screens.iter().find(|s| s.output == id) {
            return found(screen, Evidence::MappingId);
        }
        if !is_uuid(id) {
            return None;
        }
    }
    let size = stream.size.filter(|&(w, h)| w > 0 && h > 0)?;
    match stream.position.filter(|_| positions == Positions::Real) {
        Some(position) => only(screens, |s| (s.x, s.y) == position && size_matches(size, s))
            .and_then(|s| found(s, Evidence::Geometry)),
        None => only(screens, |s| {
            size_matches(size, s) || size_matches((size.1, size.0), s)
        })
        .and_then(|s| found(s, Evidence::UniqueSize)),
    }
}

/// The screen `fits` picks, when it picks exactly one.
fn only(screens: &[ScreenInfo], fits: impl Fn(&ScreenInfo) -> bool) -> Option<&ScreenInfo> {
    let mut fitting = screens.iter().filter(|s| fits(s));
    let first = fitting.next()?;
    fitting.next().is_none().then_some(first)
}

/// Shaped like a UUID (`8-4-4-4-12` hex digits), as GNOME's mapping ids
/// are: never an output's name.
fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Whether a stream of `size` can show `screen`: the screen's pixels
/// (xdph and COSMIC send the buffer's size), or its logical size, the
/// pixels over its scale (xdg-output's ratio, `sources`), within a pixel on
/// each side, as a compositor may round, floor or ceil it.
fn size_matches((w, h): (i32, i32), screen: &ScreenInfo) -> bool {
    if (i64::from(w), i64::from(h)) == (i64::from(screen.width), i64::from(screen.height)) {
        return true;
    }
    let scale = if screen.scale.is_finite() && screen.scale > 0.0 {
        screen.scale
    } else {
        1.0
    };
    let near = |given: i32, pixels: u32| (f64::from(given) - f64::from(pixels) / scale).abs() < 1.0;
    near(w, screen.width) && near(h, screen.height)
}

/// Files the portal's new restore token (`entry`) after `requested` was
/// asked for and the stream was `identified` as an output: under that
/// output, never under a guess. The requested output keeps a token only
/// when it was the one shared and the portal renewed it; otherwise its
/// token goes, so the dialog shows for it next time: the portal doesn't
/// persist, the token restored another monitor (xdg-desktop-portal hands
/// the same token back, so kept, it would restore that monitor every
/// time), or nothing could verify what it restores.
fn file_token(
    tokens: &mut PortalTokens,
    requested: &str,
    identified: Option<&str>,
    entry: Option<TokenEntry>,
) {
    let renewed = match (identified, entry) {
        (Some(output), Some(entry)) => {
            tokens.set(output, entry);
            output == requested
        }
        _ => false,
    };
    if !renewed {
        tokens.remove(requested);
    }
}

/// The screens for a log line: `DP-1 0,0 2560x1440 @1.25, …`.
fn describe_screens(screens: &[ScreenInfo]) -> String {
    if screens.is_empty() {
        return "none known".to_owned();
    }
    screens
        .iter()
        .map(|s| {
            format!(
                "{} {},{} {}x{} @{}",
                s.output, s.x, s.y, s.width, s.height, s.scale
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    use proptest::prelude::*;

    use super::Positions::{Placeholder, Real};
    use super::*;

    /// A screen as `sources` lists it: its pixels, and the scale to its
    /// logical size.
    fn screen(
        output: &str,
        (x, y): (i32, i32),
        (width, height): (u32, u32),
        scale: f64,
    ) -> ScreenInfo {
        ScreenInfo {
            output: output.into(),
            description: String::new(),
            x,
            y,
            width,
            height,
            scale,
        }
    }

    /// A monitor stream.
    fn stream(
        mapping_id: Option<&str>,
        position: Option<(i32, i32)>,
        size: Option<(i32, i32)>,
    ) -> StreamFacts {
        StreamFacts {
            mapping_id: mapping_id.map(str::to_owned),
            position,
            size,
            source_type: Some(SourceType::Monitor),
        }
    }

    fn found(output: &str, by: Evidence) -> Option<Identified> {
        Some(Identified {
            output: output.into(),
            by,
        })
    }

    fn entry(token: &str) -> TokenEntry {
        TokenEntry {
            token: token.into(),
            position: None,
            size: None,
            updated: None,
        }
    }

    /// Every token by output, for comparing.
    fn tokens_of(tokens: &PortalTokens) -> Vec<(&str, &str)> {
        tokens
            .outputs
            .iter()
            .map(|(output, entry)| (output.as_str(), entry.token.as_str()))
            .collect()
    }

    const GNOME_UUID: &str = "6f1c3a52-9b7e-4d2a-8c11-0e5f7a9b3c4d";

    /// `identify` before mapping ids: the first screen at the stream's
    /// position whose pixels or rounded logical size are the stream's.
    fn identify_by_position(stream: &StreamFacts, screens: &[ScreenInfo]) -> Option<String> {
        let position = stream.position?;
        screens
            .iter()
            .find(|s| {
                let scale = if s.scale > 0.0 { s.scale } else { 1.0 };
                let logical = (
                    (f64::from(s.width) / scale).round() as i32,
                    (f64::from(s.height) / scale).round() as i32,
                );
                position == (s.x, s.y)
                    && stream.size.is_none_or(|size| {
                        size == (s.width as i32, s.height as i32) || size == logical
                    })
            })
            .map(|s| s.output.clone())
    }

    /// Hyprland: xdph says 0,0 for every monitor, with the buffer's size,
    /// and from 1.4.0 names the monitor in the mapping id. By position,
    /// picking B of two same-size monitors identified A: B's token went
    /// under A, B could never be recorded, and a later take of A recorded B.
    #[test]
    fn hyprland_streams_are_identified_by_their_mapping_id() {
        let screens = [
            screen("DP-1", (0, 0), (2560, 1440), 1.0),
            screen("DP-2", (2560, 0), (2560, 1440), 1.0),
        ];
        let b = stream(Some("DP-2"), Some((0, 0)), Some((2560, 1440)));
        assert_eq!(
            identify_by_position(&b, &screens).as_deref(),
            Some("DP-1"),
            "the old match"
        );
        assert_eq!(
            identify(&b, &screens, Placeholder),
            found("DP-2", Evidence::MappingId)
        );
        let a = stream(Some("DP-1"), Some((0, 0)), Some((2560, 1440)));
        assert_eq!(
            identify(&a, &screens, Placeholder),
            found("DP-1", Evidence::MappingId)
        );

        // B shared for A: B's new token goes under B, and A's goes (B's all
        // along, it would restore B every time), so A's dialog shows next
        // time.
        let mut tokens = PortalTokens::default();
        tokens.set("DP-1", entry("b-under-a"));
        file_token(&mut tokens, "DP-1", Some("DP-2"), Some(entry("b")));
        assert_eq!(tokens_of(&tokens), [("DP-2", "b")]);
        // Then B restores as B.
        file_token(&mut tokens, "DP-2", Some("DP-2"), Some(entry("b2")));
        assert_eq!(tokens_of(&tokens), [("DP-2", "b2")]);
    }

    /// xdph before 1.4.0 sends no mapping id, and its 0,0 says nothing: on
    /// Hyprland a stream is the one screen of its size, either way round
    /// (its buffer is in the panel's own orientation), or none. Two
    /// monitors of one size stay unverified, never filed under each other.
    #[test]
    fn hyprland_without_a_mapping_id_goes_by_a_size_no_other_screen_has() {
        assert_eq!(Positions::of(&Desktop::Hyprland), Placeholder);
        for desktop in [
            Desktop::Niri,
            Desktop::Gnome,
            Desktop::Sway,
            Desktop::Unknown,
        ] {
            assert_eq!(Positions::of(&desktop), Real, "{desktop:?}");
        }

        // A take of DP-2: xdph says 0,0 and 2560x1440, as DP-1 would. Taken
        // at its word that was DP-1, so DP-2's token went under DP-1 and
        // DP-2 could never be recorded. Now it is shared unverified, and
        // its token kept nowhere.
        let twins = [
            screen("DP-1", (0, 0), (2560, 1440), 1.0),
            screen("DP-2", (2560, 0), (2560, 1440), 1.0),
        ];
        let b = stream(None, Some((0, 0)), Some((2560, 1440)));
        assert_eq!(
            identify(&b, &twins, Real),
            found("DP-1", Evidence::Geometry)
        );
        let identified = identify(&b, &twins, Placeholder);
        assert_eq!(identified, None);
        let mut tokens = PortalTokens::default();
        tokens.set("DP-1", entry("a"));
        tokens.set("DP-2", entry("b"));
        let identified = identified.as_ref().map(|f| f.output.as_str());
        file_token(&mut tokens, "DP-2", identified, Some(entry("b2")));
        assert_eq!(tokens_of(&tokens), [("DP-1", "a")]);

        // Monitors of other sizes are each told by their own, wherever they
        // are: at its word, 0,0 left the one off the origin unverified.
        let screens = [
            screen("DP-1", (0, 0), (2560, 1440), 1.25),
            screen("HDMI-A-1", (2048, 0), (1920, 1080), 1.0),
        ];
        for (size, output) in [((2560, 1440), "DP-1"), ((1920, 1080), "HDMI-A-1")] {
            let s = stream(None, Some((0, 0)), Some(size));
            assert_eq!(
                identify(&s, &screens, Placeholder),
                found(output, Evidence::UniqueSize)
            );
        }
        let hdmi = stream(None, Some((0, 0)), Some((1920, 1080)));
        assert_eq!(identify(&hdmi, &screens, Real), None);

        // A portrait monitor's stream is the panel's 2560x1440: next to a
        // landscape twin, that tells neither apart; next to another size,
        // it tells the portrait one.
        let portrait = screen("DP-2", (2560, 0), (1440, 2560), 1.0);
        let s = stream(None, Some((0, 0)), Some((2560, 1440)));
        let screens = [twins[0].clone(), portrait.clone()];
        assert_eq!(identify(&s, &screens, Placeholder), None);
        let screens = [screen("DP-1", (0, 0), (1920, 1080), 1.0), portrait];
        assert_eq!(
            identify(&s, &screens, Placeholder),
            found("DP-2", Evidence::UniqueSize)
        );
    }

    /// A mapping id naming no known screen (a monitor plugged in while the
    /// dialog was open) leaves the stream unidentified: xdph's 0,0 would
    /// match the screen at the origin.
    #[test]
    fn an_unknown_output_name_leaves_the_geometry_untrusted() {
        let screens = [
            screen("DP-1", (0, 0), (1920, 1080), 1.0),
            screen("DP-2", (1920, 0), (2560, 1440), 1.0),
        ];
        for positions in [Real, Placeholder] {
            let new = stream(Some("HDMI-A-1"), Some((0, 0)), Some((1920, 1080)));
            assert_eq!(identify(&new, &screens, positions), None);
            let new = stream(Some("HDMI-A-1"), None, Some((2560, 1440)));
            assert_eq!(identify(&new, &screens, positions), None);
        }
    }

    /// GNOME's mapping id (only in remote-desktop sessions) is a UUID: an
    /// unknown id that names nothing, so the geometry decides, never a
    /// refusal. Without one (screen casts) it is niri's case.
    #[test]
    fn gnome_uuids_leave_it_to_the_geometry() {
        let screens = [
            screen("eDP-1", (0, 0), (2880, 1800), 1.5),
            screen("DP-1", (1920, 0), (3840, 2160), 2.0),
        ];
        for id in [Some(GNOME_UUID), None] {
            let dp1 = stream(id, Some((1920, 0)), Some((1920, 1080)));
            assert_eq!(
                identify(&dp1, &screens, Real),
                found("DP-1", Evidence::Geometry)
            );
            let edp1 = stream(id, Some((0, 0)), Some((1920, 1200)));
            assert_eq!(
                identify(&edp1, &screens, Real),
                found("eDP-1", Evidence::Geometry)
            );
            let none = stream(id, Some((0, 0)), Some((1280, 720)));
            assert_eq!(identify(&none, &screens, Real), None);
        }
        assert!(is_uuid(GNOME_UUID));
        assert!(is_uuid(&GNOME_UUID.to_uppercase()));
        for name in [
            "DP-1",
            "HEADLESS-1",
            "",
            &GNOME_UUID[1..],
            &GNOME_UUID.replace('-', "0"),
            &GNOME_UUID.replace('d', "g"),
        ] {
            assert!(!is_uuid(name), "{name}");
        }
    }

    /// niri sends no mapping id: its logical position and size decide, and
    /// its tokens are filed, as before.
    #[test]
    fn niri_streams_are_identified_by_position_and_size_as_before() {
        // Laid out as on the dev machine: a laptop's panel, a 1440p monitor.
        let screens = [
            screen("eDP-1", (0, 240), (1920, 1200), 1.0),
            screen("DP-4", (1920, 0), (2560, 1440), 1.0),
        ];
        let cases = [
            (
                stream(None, Some((1920, 0)), Some((2560, 1440))),
                Some("DP-4"),
            ),
            (
                stream(None, Some((0, 240)), Some((1920, 1200))),
                Some("eDP-1"),
            ),
            (stream(None, Some((0, 0)), Some((1920, 1200))), None),
            (stream(None, Some((1920, 0)), Some((1920, 1200))), None),
        ];
        for (stream, output) in cases {
            let identified = identify(&stream, &screens, Real);
            assert_eq!(identified.as_ref().map(|f| f.output.as_str()), output);
            assert!(identified.is_none_or(|f| f.by == Evidence::Geometry));
            assert_eq!(identify_by_position(&stream, &screens).as_deref(), output);
        }

        let mut tokens = PortalTokens::default();
        tokens.set("DP-4", entry("old"));
        tokens.set("eDP-1", entry("panel"));
        file_token(&mut tokens, "DP-4", Some("DP-4"), Some(entry("new")));
        assert_eq!(tokens_of(&tokens), [("DP-4", "new"), ("eDP-1", "panel")]);
        // A portal that doesn't persist drops the token, as before.
        file_token(&mut tokens, "DP-4", Some("DP-4"), None);
        assert_eq!(tokens_of(&tokens), [("eDP-1", "panel")]);
    }

    /// Under fractional scaling a stream's logical size is the pixels over
    /// the screen's xdg-output ratio, rounded (niri, GNOME) or floored
    /// (wlroots); the buffer's pixels (xdph, COSMIC) match too.
    #[test]
    fn fractional_scales_match_the_logical_size() {
        let screens = [
            screen("eDP-1", (0, 0), (2560, 1600), 1.25),
            screen("DP-1", (2048, 0), (2880, 1800), 1.5),
        ];
        let by_size = [
            ((0, 0), (2048, 1280), Some("eDP-1")),
            ((0, 0), (2560, 1600), Some("eDP-1")),
            ((2048, 0), (1920, 1200), Some("DP-1")),
            ((2048, 0), (2880, 1800), Some("DP-1")),
            // Two pixels off on both sides, or the wrong screen's size.
            ((2048, 0), (1918, 1202), None),
            ((0, 0), (1920, 1200), None),
        ];
        for (position, size, output) in by_size {
            let identified = identify(&stream(None, Some(position), Some(size)), &screens, Real);
            assert_eq!(
                identified.map(|f| f.output),
                output.map(str::to_owned),
                "{size:?} at {position:?}"
            );
        }

        // 2560×1440 at 1.5 is 1706.67×960 logical: niri rounds it to 1707
        // (scale 1.5), sway floors it to 1706 (scale 2560 ÷ 1706).
        for (scale, size) in [(1.5, (1707, 960)), (2560.0 / 1706.0, (1706, 960))] {
            let screens = [screen("DP-4", (0, 0), (2560, 1440), scale)];
            let s = stream(None, Some((0, 0)), Some(size));
            assert_eq!(
                identify(&s, &screens, Real),
                found("DP-4", Evidence::Geometry)
            );
        }
        // 1366×768 at 1.25, floored by sway to 1092×614 (scale 1366 ÷ 1092):
        // 768 ÷ that is 613.95.
        let screens = [screen("LVDS-1", (0, 0), (1366, 768), 1366.0 / 1092.0)];
        let s = stream(None, Some((0, 0)), Some((1092, 614)));
        assert_eq!(
            identify(&s, &screens, Real),
            found("LVDS-1", Evidence::Geometry)
        );
        // A scale the listing can't have reads as 1.
        let screens = [screen("X-1", (0, 0), (1920, 1080), f64::NAN)];
        let s = stream(None, Some((0, 0)), Some((1920, 1080)));
        assert_eq!(
            identify(&s, &screens, Real),
            found("X-1", Evidence::Geometry)
        );
    }

    /// A stream with no position is the one screen of its size, either way
    /// round, if only one has it; a position alone (no size) says nothing.
    #[test]
    fn without_a_position_a_size_only_one_screen_has_decides() {
        let screens = [
            screen("DP-1", (0, 0), (2560, 1440), 1.0),
            screen("DP-2", (2560, 0), (1920, 1080), 1.0),
            screen("DP-3", (4480, 0), (3840, 2160), 2.0),
        ];
        let cases = [
            (
                stream(None, None, Some((2560, 1440))),
                found("DP-1", Evidence::UniqueSize),
            ),
            // DP-2's pixels and DP-3's logical size.
            (stream(None, None, Some((1920, 1080))), None),
            (
                stream(None, None, Some((3840, 2160))),
                found("DP-3", Evidence::UniqueSize),
            ),
            (stream(None, None, Some((1280, 720))), None),
            // Either way round, as a rotated panel's buffer is.
            (
                stream(None, None, Some((1440, 2560))),
                found("DP-1", Evidence::UniqueSize),
            ),
            (stream(None, None, Some((1080, 1920))), None),
            (stream(None, None, None), None),
            (stream(None, Some((0, 0)), None), None),
            (
                stream(Some(GNOME_UUID), None, Some((2560, 1440))),
                found("DP-1", Evidence::UniqueSize),
            ),
        ];
        for (stream, expected) in cases {
            for positions in [Real, Placeholder] {
                assert_eq!(identify(&stream, &screens, positions), expected, "{stream}");
            }
        }
    }

    /// No screens known, a stream that isn't a monitor, mirrored screens,
    /// an empty size: nothing to tell by.
    #[test]
    fn nothing_identifies_what_nothing_tells_apart() {
        let s = stream(None, Some((0, 0)), Some((1920, 1080)));
        assert_eq!(identify(&s, &[], Real), None);
        let one = [screen("DP-1", (0, 0), (1920, 1080), 1.0)];
        assert_eq!(identify(&s, &one, Real), found("DP-1", Evidence::Geometry));
        for kind in [SourceType::Window, SourceType::Virtual] {
            let not_a_monitor = StreamFacts {
                source_type: Some(kind),
                ..s.clone()
            };
            assert_eq!(identify(&not_a_monitor, &one, Real), None, "{kind:?}");
        }
        let unknown_kind = StreamFacts {
            source_type: None,
            ..s.clone()
        };
        assert_eq!(
            identify(&unknown_kind, &one, Real),
            found("DP-1", Evidence::Geometry)
        );
        // An empty mapping id is none.
        let empty_id = stream(Some(""), Some((0, 0)), Some((1920, 1080)));
        assert_eq!(
            identify(&empty_id, &one, Real),
            found("DP-1", Evidence::Geometry)
        );
        // COSMIC without the output's info: 0,0 and 0×0.
        let empty = stream(None, Some((0, 0)), Some((0, 0)));
        assert_eq!(identify(&empty, &one, Real), None);

        let mirrored = [
            screen("DP-1", (0, 0), (1920, 1080), 1.0),
            screen("HDMI-A-1", (0, 0), (1920, 1080), 1.0),
        ];
        assert_eq!(identify(&s, &mirrored, Real), None);
        let named = stream(Some("HDMI-A-1"), Some((0, 0)), Some((1920, 1080)));
        assert_eq!(
            identify(&named, &mirrored, Real),
            found("HDMI-A-1", Evidence::MappingId)
        );
    }

    /// The new token goes under the output identified, never under a
    /// guess, and the requested output keeps one only when it was shared
    /// and the portal renewed it.
    #[test]
    fn tokens_are_filed_under_the_output_shared_never_a_guess() {
        /// The tokens by output.
        type Filed = &'static [(&'static str, &'static str)];
        // DP-1 was asked for: (identified, the portal's new token, after)
        let cases: [(Option<&str>, Option<&str>, Filed); 6] = [
            (
                Some("DP-1"),
                Some("new"),
                &[("DP-1", "new"), ("DP-2", "two")],
            ),
            (Some("DP-1"), None, &[("DP-2", "two")]),
            (Some("DP-2"), Some("new"), &[("DP-2", "new")]),
            (Some("DP-2"), None, &[("DP-2", "two")]),
            (None, Some("new"), &[("DP-2", "two")]),
            (None, None, &[("DP-2", "two")]),
        ];
        for (identified, new, after) in cases {
            let mut tokens = PortalTokens::default();
            tokens.set("DP-1", entry("one"));
            tokens.set("DP-2", entry("two"));
            file_token(&mut tokens, "DP-1", identified, new.map(entry));
            assert_eq!(tokens_of(&tokens), after, "{identified:?} {new:?}");
        }
    }

    #[test]
    fn streams_and_screens_read_well_in_the_log() {
        let s = stream(Some("DP-2"), Some((0, 0)), Some((2560, 1440)));
        assert_eq!(
            s.to_string(),
            r#"mapping id "DP-2", position 0,0, size 2560x1440"#
        );
        let s = StreamFacts {
            source_type: Some(SourceType::Window),
            ..StreamFacts::default()
        };
        assert_eq!(
            s.to_string(),
            "no mapping id, no position, no size, a Window stream"
        );
        assert_eq!(describe_screens(&[]), "none known");
        let screens = [
            screen("eDP-1", (0, 240), (1920, 1200), 1.0),
            screen("DP-4", (1920, 0), (2560, 1440), 1.25),
        ];
        assert_eq!(
            describe_screens(&screens),
            "eDP-1 0,240 1920x1200 @1, DP-4 1920,0 2560x1440 @1.25"
        );

        // On Hyprland without a mapping id, the line says which portal
        // would tell: elsewhere, or with an id, nothing more.
        let anonymous = stream(None, Some((0, 0)), Some((2560, 1440)));
        assert_eq!(
            Placeholder.unverified_hint(&anonymous),
            " (on Hyprland, only xdg-desktop-portal-hyprland 1.4.0 and later say which \
             monitor they share)"
        );
        let named = stream(Some("HDMI-A-1"), Some((0, 0)), Some((2560, 1440)));
        assert_eq!(Placeholder.unverified_hint(&named), "");
        assert_eq!(Real.unverified_hint(&anonymous), "");
    }

    const NAMES: [&str; 4] = ["DP-1", "DP-2", "HDMI-A-1", "eDP-1"];
    const POSITIONS: [(i32, i32); 4] = [(0, 0), (1920, 0), (2560, 0), (0, 1080)];
    /// Pixel sizes, a rotated (portrait) monitor's among them.
    const MODES: [(u32, u32); 5] = [
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
        (1366, 768),
        (1440, 2560),
    ];
    const SCALES: [f64; 4] = [1.0, 1.25, 1.5, 2.0];

    /// Up to four screens, each name once, from small pools so streams and
    /// screens often agree: same positions (mirrors), same sizes, the same
    /// logical size at two scales, a portrait size that is a landscape one
    /// turned.
    fn screens_strategy(distinct_positions: bool) -> impl Strategy<Value = Vec<ScreenInfo>> {
        let positions = if distinct_positions {
            Just(POSITIONS.to_vec()).prop_shuffle().boxed()
        } else {
            proptest::collection::vec(proptest::sample::select(POSITIONS.to_vec()), 4).boxed()
        };
        (
            proptest::sample::subsequence(NAMES.to_vec(), 0..=4),
            positions,
            proptest::collection::vec((0..MODES.len(), 0..SCALES.len()), 4),
        )
            .prop_map(|(names, positions, looks)| {
                names
                    .iter()
                    .zip(positions)
                    .zip(looks)
                    .map(|((name, position), (mode, scale))| {
                        screen(name, position, MODES[mode], SCALES[scale])
                    })
                    .collect()
            })
    }

    /// A pool mode's logical size at a pool scale (rounded or floored; 1
    /// gives its pixels), or any size.
    fn size_strategy() -> impl Strategy<Value = (i32, i32)> {
        prop_oneof![
            3 => (0..MODES.len(), 0..SCALES.len(), any::<bool>()).prop_map(
                |(mode, scale, floor)| {
                    let (w, h) = MODES[mode];
                    let logical = |p: u32| {
                        let v = f64::from(p) / SCALES[scale];
                        (if floor { v.floor() } else { v.round() }) as i32
                    };
                    (logical(w), logical(h))
                }
            ),
            1 => (-1..4000i32, -1..3000i32),
        ]
    }

    /// A stream's facts from the same pools: a known or unknown name, a
    /// UUID, an empty id or none; a pool position or none; a size or none;
    /// any source type.
    fn stream_strategy() -> impl Strategy<Value = StreamFacts> {
        let id = prop_oneof![
            Just(None),
            proptest::sample::select(NAMES.to_vec()).prop_map(|n| Some(n.to_owned())),
            Just(Some("HEADLESS-9".to_owned())),
            Just(Some(GNOME_UUID.to_owned())),
            Just(Some(String::new())),
        ];
        let position = proptest::option::of(proptest::sample::select(POSITIONS.to_vec()));
        let size = proptest::option::of(size_strategy());
        let source_type = proptest::option::of(proptest::sample::select(vec![
            SourceType::Monitor,
            SourceType::Window,
            SourceType::Virtual,
        ]));
        (id, position, size, source_type).prop_map(|(mapping_id, position, size, source_type)| {
            StreamFacts {
                mapping_id,
                position,
                size,
                source_type,
            }
        })
    }

    /// Screens, and a stream that often takes the position of one of them,
    /// and its pixels or its rounded or floored logical size, sometimes
    /// with its sides swapped (xdph's buffer of a rotated panel).
    fn case_strategy() -> impl Strategy<Value = (Vec<ScreenInfo>, StreamFacts)> {
        (
            screens_strategy(false),
            stream_strategy(),
            any::<proptest::sample::Index>(),
            0u8..32,
        )
            .prop_map(|(screens, mut stream, pick, how)| {
                if !screens.is_empty() {
                    let s = &screens[pick.index(screens.len())];
                    if how & 1 != 0 {
                        stream.position = Some((s.x, s.y));
                    }
                    let logical = |p: u32| {
                        let v = f64::from(p) / s.scale;
                        (if how & 4 != 0 { v.floor() } else { v.round() }) as i32
                    };
                    if how & 2 != 0 {
                        let (w, h) = if how & 8 != 0 {
                            (s.width as i32, s.height as i32)
                        } else {
                            (logical(s.width), logical(s.height))
                        };
                        stream.size = Some(if how & 16 != 0 { (h, w) } else { (w, h) });
                    }
                }
                (screens, stream)
            })
    }

    fn positions_strategy() -> impl Strategy<Value = Positions> {
        prop_oneof![Just(Real), Just(Placeholder)]
    }

    /// The test's own reading of "a stream of this size can show this
    /// screen": its pixels, or within a pixel of pixels ÷ scale.
    fn shows(size: (i32, i32), s: &ScreenInfo) -> bool {
        let logical = |p: u32| f64::from(p) / if s.scale > 0.0 { s.scale } else { 1.0 };
        size == (s.width as i32, s.height as i32)
            || ((f64::from(size.0) - logical(s.width)).abs() < 1.0
                && (f64::from(size.1) - logical(s.height)).abs() < 1.0)
    }

    proptest! {
        /// An identified stream is a known screen, named by its mapping id
        /// or, when the id names no screen (none, or a UUID), the only one
        /// with its geometry; a known name always wins, an unknown name
        /// identifies nothing, a UUID is as no id at all, and a placeholder
        /// position as none.
        #[test]
        fn a_result_always_matches_by_mapping_id_or_geometry(
            (screens, stream) in case_strategy(),
            positions in positions_strategy(),
        ) {
            let result = identify(&stream, &screens, positions);
            let id = stream.mapping_id.as_deref().filter(|id| !id.is_empty());
            let named = id.and_then(|id| screens.iter().find(|s| s.output == id));
            let monitor = stream.source_type.is_none_or(|k| k == SourceType::Monitor);
            match &result {
                Some(found) => {
                    prop_assert!(monitor);
                    let screen = screens.iter().find(|s| s.output == found.output);
                    prop_assert!(screen.is_some(), "{} isn't a known screen", found.output);
                    let screen = screen.unwrap();
                    let size = stream.size.unwrap_or_default();
                    match found.by {
                        Evidence::MappingId => prop_assert_eq!(id, Some(screen.output.as_str())),
                        Evidence::Geometry => {
                            prop_assert!(named.is_none());
                            prop_assert!(id.is_none_or(is_uuid));
                            prop_assert_eq!(positions, Real);
                            let at = |s: &&ScreenInfo| Some((s.x, s.y)) == stream.position;
                            prop_assert!(at(&screen) && shows(size, screen));
                            let fits = screens.iter().filter(|s| at(s) && shows(size, s));
                            prop_assert_eq!(fits.count(), 1);
                        }
                        Evidence::UniqueSize => {
                            prop_assert!(named.is_none());
                            prop_assert!(id.is_none_or(is_uuid));
                            prop_assert!(stream.position.is_none() || positions == Placeholder);
                            let fits = |s: &ScreenInfo| shows(size, s) || shows((size.1, size.0), s);
                            prop_assert!(fits(screen));
                            prop_assert_eq!(screens.iter().filter(|s| fits(s)).count(), 1);
                        }
                    }
                }
                None => {
                    prop_assert!(!monitor || named.is_none());
                }
            }
            if monitor && let Some(screen) = named {
                prop_assert_eq!(result.clone(), found(&screen.output, Evidence::MappingId));
            }
            if id.is_some_and(|id| named.is_none() && !is_uuid(id)) {
                prop_assert_eq!(result.clone(), None);
            }
            if id.is_some_and(is_uuid) {
                let anonymous = StreamFacts { mapping_id: None, ..stream.clone() };
                prop_assert_eq!(result.clone(), identify(&anonymous, &screens, positions));
            }
            // xdph's 0,0 says nothing: the answer is the one without it.
            if positions == Placeholder {
                let nowhere = StreamFacts { position: None, ..stream.clone() };
                prop_assert_eq!(result, identify(&nowhere, &screens, Placeholder));
            }
        }

        /// Whatever was shared, the portal's new token never lands under an
        /// output the stream wasn't identified as: with no identification
        /// it is stored nowhere and the requested output's token goes; the
        /// other outputs' tokens stay.
        #[test]
        fn no_token_is_stored_when_nothing_identified_the_stream(
            (screens, stream) in case_strategy(),
            positions in positions_strategy(),
            held in proptest::sample::subsequence(NAMES.to_vec(), 0..=4),
            requested in proptest::sample::select(NAMES.to_vec()),
            renewed in any::<bool>(),
        ) {
            let mut tokens = PortalTokens::default();
            for output in &held {
                tokens.set(output, entry(&format!("old {output}")));
            }
            let before = tokens.clone();
            let result = identify(&stream, &screens, positions);
            let identified = result.as_ref().map(|f| f.output.as_str());
            file_token(&mut tokens, requested, identified, renewed.then(|| entry("new")));
            let holders: Vec<&str> = tokens
                .outputs
                .iter()
                .filter(|(_, e)| e.token == "new")
                .map(|(output, _)| output.as_str())
                .collect();
            match identified {
                None => {
                    prop_assert!(holders.is_empty());
                    prop_assert!(tokens.get(requested).is_none());
                }
                Some(output) => {
                    let expected: &[&str] = if renewed { &[output] } else { &[] };
                    prop_assert_eq!(holders, expected);
                    if output != requested || !renewed {
                        prop_assert!(tokens.get(requested).is_none());
                    }
                }
            }
            for (output, old) in &before.outputs {
                let replaced = renewed && Some(output.as_str()) == identified;
                if output != requested && !replaced {
                    prop_assert_eq!(tokens.get(output), Some(old));
                }
            }
        }

        /// niri (no mapping id, a real logical position and size, no two
        /// outputs at one position): every stream the position match took
        /// is still taken, as the same screen.
        #[test]
        fn streams_without_a_mapping_id_match_as_before(
            screens in screens_strategy(true),
            position in proptest::sample::select(POSITIONS.to_vec()),
            size in size_strategy(),
        ) {
            let stream = stream(None, Some(position), Some(size));
            if let Some(before) = identify_by_position(&stream, &screens) {
                let now = identify(&stream, &screens, Real);
                prop_assert_eq!(now, found(&before, Evidence::Geometry));
            }
        }
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
