//! The stage (G§4.6, C06–C08): its geometry, layer boxes and their edits,
//! the overlays (countdown, REC, transport, badges) and the first-run
//! source list. Owner: WP4.
//!
//! - **Geometry** ([`stage_geometry`]): the preview is the canvas aspect
//!   fitted into `(stage_w − 32) × (stage_h − 68)`, centered in x ∈ [16,
//!   w − 16] and y ∈ [16, h − 52]; the 36 px transport row is anchored to
//!   the stage bottom (top = h − 44) under the preview's x and width.
//! - **Layer boxes** ([`layer_boxes`]): `layout::place`'s destination of
//!   each layer × preview / canvas, preview-local, from the sources' sizes
//!   ([`source_sizes`]: the live status in Live, the take's streams in
//!   Program, the rect while unknown): what a press hits. The selection
//!   box and its handles are the layer's rect (its `frame`), what the
//!   edits change (R20).
//! - **Edits** (D-09, D-32, D-33): [`hit`] (the selected box's handles, then
//!   the topmost visible box), [`drag`] (move, resize, crop, snapped to the
//!   canvas' edges, thirds and center), [`nudge`] for nudge bursts, and the
//!   position [`readout`].
//! - **Overlays**: [`countdown`], [`rec`], [`transport`], [`badges`], the
//!   S10 [`trim_plate`].
//! - **First run** (S01a, D-16): [`first_run`] (when: Live, an empty
//!   scene), [`first_run_rows`], [`with_choices`] (the person's picks
//!   across a refreshed inventory), [`first_run_commands`],
//!   [`add_sources_label`].
//!
//! Geometry is in logical px at 100 % UI scale (D-46); canvas edits are in
//! canvas px.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ctx::{AudioDeviceInfo, CameraInfo, ScreenInfo};
use crate::ids::{Frame, LayerId, TakeId};
use crate::layout::place;
use crate::model::{AudioRole, Canvas, Crop, Fit, Layer, Project, Scene, VideoSource};
use crate::protocol::command::{Command, SceneTarget, VideoSourceArg};
use crate::state::{CanvasMode, RecordPhase, State};
use crate::time::ns_to_frame_floor;
use crate::timeline::total_frames;
use crate::view::format;
use crate::view::{LiveSourceState, LiveSources, Rect};

/// The scene layout edits go to (D-06): `ui_normalize::edit_target`. `None`
/// in Program mode with nothing selected: rows, inspector and canvas
/// drags are disabled.
pub fn edit_target(state: &State) -> Option<SceneTarget> {
    crate::ui_normalize::edit_target(&state.project, &state.ui)
}

/// The scene `target` names, if it exists.
pub fn scene_of<'a>(project: &'a Project, target: &SceneTarget) -> Option<&'a Scene> {
    match target {
        SceneTarget::Project => Some(&project.scene),
        SceneTarget::Take(take) => project.take(take).map(|t| &t.scene),
    }
}

// ------------------------------------------------------------- geometry

/// The stage's padding around the preview (G§3.15).
pub const STAGE_PAD: f64 = 16.0;
/// The transport row's height and its distance from the stage bottom.
pub const TRANSPORT_H: f64 = 36.0;
const TRANSPORT_BOTTOM: f64 = 44.0;
/// What the preview leaves below it for the transport: 68 − 16.
const BELOW_PREVIEW: f64 = 52.0;

/// Where the preview and the transport row sit in the stage (stage-local).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageGeometry {
    pub preview: Rect,
    pub transport: Rect,
}

/// The canvas aspect fitted into `(stage_w − 32) × (stage_h − 68)`; the
/// 36 px transport row anchored to the stage bottom (G§3.15).
pub fn stage_geometry(stage_w: f64, stage_h: f64, canvas: &Canvas) -> StageGeometry {
    let avail_w = (stage_w - 2.0 * STAGE_PAD).max(0.0);
    let avail_h = (stage_h - STAGE_PAD - BELOW_PREVIEW).max(0.0);
    let (cw, ch) = (
        f64::from(canvas.width.max(1)),
        f64::from(canvas.height.max(1)),
    );
    let scale = (avail_w / cw).min(avail_h / ch);
    let (w, h) = (cw * scale, ch * scale);
    let x = STAGE_PAD + (avail_w - w) / 2.0;
    let y = STAGE_PAD + (avail_h - h) / 2.0;
    StageGeometry {
        preview: Rect::new(x, y, w, h),
        transport: Rect::new(x, stage_h - TRANSPORT_BOTTOM, w, TRANSPORT_H),
    }
}

// ----------------------------------------------------------- layer boxes

/// A layer's box on the preview (C08).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerBox {
    pub layer: LayerId,
    /// The picture, preview-local (`layout::place`'s dst): what a press on
    /// the body hits.
    pub rect: Rect,
    /// The layer's rect, preview-local: the selection box and its handles
    /// (what a resize, a move's snapping and the readout act on). It is
    /// the picture's box but for a Contain layer whose source has another
    /// aspect (R20: the handles sat on the fitted picture while the edits
    /// moved the rect's edges, so a grabbed handle drifted off the pointer).
    pub frame: Rect,
    pub visible: bool,
    pub locked: bool,
    /// Array order: the last is on top.
    pub z: usize,
}

/// The boxes of `scene`'s layers on a preview at `preview`; `sizes` are
/// the sources' sizes (Live: the live status; Program: the take's streams;
/// unknown: the rect).
pub fn layer_boxes(
    scene: &Scene,
    canvas: &Canvas,
    sizes: &BTreeMap<LayerId, (u32, u32)>,
    preview: Rect,
) -> Vec<LayerBox> {
    let scale = Scale::of(canvas, preview);
    scene
        .layers
        .iter()
        .enumerate()
        .map(|(z, layer)| {
            let rect = scale.rect(placed(
                layer.rect,
                layer.crop,
                layer.fit,
                sizes.get(&layer.id).copied(),
            ));
            LayerBox {
                layer: layer.id.clone(),
                rect,
                frame: scale.rect(layer.rect),
                visible: layer.visible,
                locked: layer.locked,
                z,
            }
        })
        .collect()
}

/// Where a layer's picture lands on the canvas: `layout::place`'s
/// destination, or the rect itself while the source's size is unknown (or
/// nothing would be visible).
pub fn placed(
    rect: crate::model::Rect,
    crop: Crop,
    fit: Fit,
    size: Option<(u32, u32)>,
) -> crate::model::Rect {
    size.and_then(|(w, h)| place(w, h, crop, rect, fit))
        .map_or(rect, |p| p.dst)
}

/// The preview's scale from canvas px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    pub x: f64,
    pub y: f64,
}

impl Scale {
    /// Canvas px → preview px for a preview of `preview`'s size.
    pub fn of(canvas: &Canvas, preview: Rect) -> Self {
        Self {
            x: preview.w / f64::from(canvas.width.max(1)),
            y: preview.h / f64::from(canvas.height.max(1)),
        }
    }

    /// A canvas rect on the preview (preview-local).
    pub fn rect(self, r: crate::model::Rect) -> Rect {
        Rect::new(
            f64::from(r.x) * self.x,
            f64::from(r.y) * self.y,
            f64::from(r.w) * self.x,
            f64::from(r.h) * self.y,
        )
    }
}

/// The source name a layer's live status is keyed by: a screen's output, a
/// camera's device.
pub fn source_name(source: &VideoSource) -> &str {
    match source {
        VideoSource::Screen { output, .. } => output,
        VideoSource::Camera { device, .. } => device,
    }
}

/// The source size of each layer of `target`'s scene: the live status's
/// in the project scene (Live), the take's recorded streams in a take scene
/// (Program). Layers whose size isn't known yet are left out.
pub fn source_sizes(
    project: &Project,
    target: &SceneTarget,
    live: &LiveSources,
) -> BTreeMap<LayerId, (u32, u32)> {
    let mut sizes = BTreeMap::new();
    match target {
        SceneTarget::Project => {
            for layer in &project.scene.layers {
                if let Some(size) = live
                    .get(source_name(&layer.source))
                    .and_then(|view| view.size)
                {
                    sizes.insert(layer.id.clone(), size);
                }
            }
        }
        SceneTarget::Take(take) => {
            let Some(take) = project.take(take) else {
                return sizes;
            };
            for layer in &take.scene.layers {
                if let Some(stream) = take.stream(layer.id.as_str())
                    && let (Some(w), Some(h)) = (stream.width, stream.height)
                {
                    sizes.insert(layer.id.clone(), (w, h));
                }
            }
        }
    }
    sizes
}

// ------------------------------------------------------------- hit test

/// A selection handle's size (C08), and the area around it that grabs it.
pub const HANDLE: f64 = 8.0;
pub const HANDLE_HIT: f64 = 12.0;

/// A handle of the selection box: the corners resize keeping the aspect
/// (`Shift` frees it), the edges one axis; with `Alt` the edges crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// A side of a layer (the crop brackets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// Where the handle sits on `r`: (x, y) as fractions of the box.
    fn at(self) -> (f64, f64) {
        match self {
            Handle::TopLeft => (0.0, 0.0),
            Handle::Top => (0.5, 0.0),
            Handle::TopRight => (1.0, 0.0),
            Handle::Right => (1.0, 0.5),
            Handle::BottomRight => (1.0, 1.0),
            Handle::Bottom => (0.5, 1.0),
            Handle::BottomLeft => (0.0, 1.0),
            Handle::Left => (0.0, 0.5),
        }
    }

    /// The handle's center on box `r`.
    pub fn center(self, r: Rect) -> (f64, f64) {
        let (fx, fy) = self.at();
        (r.x + r.w * fx, r.y + r.h * fy)
    }

    /// The 8×8 square drawn for it (or `size`×`size`).
    pub fn square(self, r: Rect, size: f64) -> Rect {
        let (cx, cy) = self.center(r);
        Rect::new(cx - size / 2.0, cy - size / 2.0, size, size)
    }

    pub fn is_corner(self) -> bool {
        matches!(
            self,
            Handle::TopLeft | Handle::TopRight | Handle::BottomRight | Handle::BottomLeft
        )
    }

    /// The side an edge handle sits on (`None` for corners).
    pub fn edge(self) -> Option<Edge> {
        match self {
            Handle::Top => Some(Edge::Top),
            Handle::Right => Some(Edge::Right),
            Handle::Bottom => Some(Edge::Bottom),
            Handle::Left => Some(Edge::Left),
            _ => None,
        }
    }

    /// Its pointer cursor (GDK names, G§5.2).
    pub fn cursor(self) -> &'static str {
        match self {
            Handle::TopLeft | Handle::BottomRight => "nwse-resize",
            Handle::TopRight | Handle::BottomLeft => "nesw-resize",
            Handle::Left | Handle::Right => "ew-resize",
            Handle::Top | Handle::Bottom => "ns-resize",
        }
    }

    /// Which way it grows the box: −1 (the left or top side), 0, +1.
    fn growth(self) -> (f64, f64) {
        let (fx, fy) = self.at();
        (fx * 2.0 - 1.0, fy * 2.0 - 1.0)
    }
}

/// What a press on the preview lands on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Hit {
    /// Nothing to select: the press deselects.
    None,
    /// A layer's picture: select, then move.
    Body { layer: LayerId },
    /// One of the selected layer's handles.
    Handle { layer: LayerId, handle: Handle },
}

/// The selected box's handles first (each grabs a 12 px square on its
/// frame, the rect), then the topmost visible picture under the point
/// (preview-local).
pub fn hit(boxes: &[LayerBox], selected: Option<&LayerId>, x: f64, y: f64) -> Hit {
    if let Some(sel) = selected.and_then(|id| boxes.iter().find(|b| &b.layer == id && b.visible)) {
        for handle in Handle::ALL {
            if handle.square(sel.frame, HANDLE_HIT).contains(x, y) {
                return Hit::Handle {
                    layer: sel.layer.clone(),
                    handle,
                };
            }
        }
    }
    boxes
        .iter()
        .rev()
        .find(|b| b.visible && b.rect.contains(x, y))
        .map_or(Hit::None, |b| Hit::Body {
            layer: b.layer.clone(),
        })
}

// ------------------------------------------------------------- snapping

/// How close (in preview px) a feature snaps to a line (D-09).
pub const SNAP_PX: f64 = 8.0;
/// The smallest a layer can be resized to (canvas px).
pub const MIN_SIZE: u32 = 16;
/// What a crop always leaves of the source (source px).
pub const MIN_CROP_SOURCE: u32 = 16;

/// A canvas axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    X,
    Y,
}

/// A snap guide shown while dragging: a dashed line across the preview at
/// canvas coordinate `at` with its tag ("Right third").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guide {
    pub axis: Axis,
    /// Canvas px along `axis`.
    pub at: f64,
    pub label: String,
}

/// The lines features snap to on an axis of length `len`: the edges, the
/// thirds and the center (D-09).
fn snap_lines(axis: Axis, len: f64) -> [(f64, &'static str); 5] {
    match axis {
        Axis::X => [
            (0.0, "Left edge"),
            (len / 3.0, "Left third"),
            (len / 2.0, "Center"),
            (len * 2.0 / 3.0, "Right third"),
            (len, "Right edge"),
        ],
        Axis::Y => [
            (0.0, "Top edge"),
            (len / 3.0, "Top third"),
            (len / 2.0, "Middle"),
            (len * 2.0 / 3.0, "Bottom third"),
            (len, "Bottom edge"),
        ],
    }
}

/// The nearest (shift, line) within `threshold` for any of `features` on
/// `axis`: what to add to the features so the nearest one sits on its line.
pub fn snap_axis(axis: Axis, len: f64, features: &[f64], threshold: f64) -> Option<(f64, Guide)> {
    let mut best: Option<(f64, Guide)> = None;
    for &feature in features {
        for (line, label) in snap_lines(axis, len) {
            let shift = line - feature;
            if shift.abs() > threshold {
                continue;
            }
            if best.as_ref().is_none_or(|(s, _)| shift.abs() < s.abs()) {
                best = Some((
                    shift,
                    Guide {
                        axis,
                        at: line,
                        label: label.to_owned(),
                    },
                ));
            }
        }
    }
    best
}

/// Snapping on a canvas, with the threshold in canvas px (8 preview px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub width: f64,
    pub height: f64,
    pub threshold_x: f64,
    pub threshold_y: f64,
}

impl Snap {
    /// Snapping at [`SNAP_PX`] preview px on `canvas` shown at `scale`.
    pub fn new(canvas: &Canvas, scale: Scale) -> Self {
        Self {
            width: f64::from(canvas.width),
            height: f64::from(canvas.height),
            threshold_x: SNAP_PX / scale.x.max(1e-9),
            threshold_y: SNAP_PX / scale.y.max(1e-9),
        }
    }
}

// ----------------------------------------------------------------- edits

/// What a drag on the canvas does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DragOp {
    /// The picture: moves the rect.
    Move,
    /// A handle: resizes (a corner keeps the aspect unless `Shift`).
    Resize { handle: Handle },
    /// `Alt` + an edge handle: crops that side.
    Crop { edge: Edge },
}

impl DragOp {
    /// The op a press on `handle` starts (`alt`: edges crop).
    pub fn for_handle(handle: Handle, alt: bool) -> Self {
        match handle.edge() {
            Some(edge) if alt => DragOp::Crop { edge },
            _ => DragOp::Resize { handle },
        }
    }
}

/// The layer as the drag found it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragStart {
    pub rect: crate::model::Rect,
    pub crop: Crop,
    pub fit: Fit,
    /// The source's size, when known (crops need it).
    pub source: Option<(u32, u32)>,
}

impl DragStart {
    pub fn of(layer: &Layer, source: Option<(u32, u32)>) -> Self {
        Self {
            rect: layer.rect,
            crop: layer.crop,
            fit: layer.fit,
            source,
        }
    }
}

/// A drag's result so far: the layer's rect and crop, and the guides it
/// snapped to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragFrame {
    pub rect: crate::model::Rect,
    pub crop: Crop,
    pub guides: Vec<Guide>,
}

/// The layer after a drag of `(dx, dy)` canvas px from where it started:
/// a move (snapped), a resize (a corner keeps the aspect unless
/// `free_aspect`; at least [`MIN_SIZE`]; the moving edges snapped) or a
/// crop (`delta × source / dst` on that side, leaving at least
/// [`MIN_CROP_SOURCE`] source px). `snap`: `None` when snapping is off.
pub fn drag(
    start: &DragStart,
    op: DragOp,
    dx: f64,
    dy: f64,
    free_aspect: bool,
    snap: Option<&Snap>,
) -> DragFrame {
    match op {
        DragOp::Move => drag_move(start, dx, dy, snap),
        DragOp::Resize { handle } => drag_resize(start, handle, dx, dy, free_aspect, snap),
        DragOp::Crop { edge } => DragFrame {
            rect: start.rect,
            crop: drag_crop(start, edge, dx, dy),
            guides: Vec::new(),
        },
    }
}

fn round_i32(v: f64) -> i32 {
    v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn round_size(v: f64) -> u32 {
    v.round().clamp(f64::from(MIN_SIZE), f64::from(u32::MAX)) as u32
}

fn drag_move(start: &DragStart, dx: f64, dy: f64, snap: Option<&Snap>) -> DragFrame {
    let r = start.rect;
    let mut x = f64::from(r.x) + dx;
    let mut y = f64::from(r.y) + dy;
    let (w, h) = (f64::from(r.w), f64::from(r.h));
    let mut guides = Vec::new();
    if let Some(snap) = snap {
        if let Some((shift, guide)) = snap_axis(
            Axis::X,
            snap.width,
            &[x, x + w / 2.0, x + w],
            snap.threshold_x,
        ) {
            x += shift;
            guides.push(guide);
        }
        if let Some((shift, guide)) = snap_axis(
            Axis::Y,
            snap.height,
            &[y, y + h / 2.0, y + h],
            snap.threshold_y,
        ) {
            y += shift;
            guides.push(guide);
        }
    }
    DragFrame {
        rect: crate::model::Rect::new(round_i32(x), round_i32(y), r.w, r.h),
        crop: start.crop,
        guides,
    }
}

fn drag_resize(
    start: &DragStart,
    handle: Handle,
    dx: f64,
    dy: f64,
    free_aspect: bool,
    snap: Option<&Snap>,
) -> DragFrame {
    let r = start.rect;
    let (x0, y0) = (f64::from(r.x), f64::from(r.y));
    let (w0, h0) = (f64::from(r.w.max(1)), f64::from(r.h.max(1)));
    let (gx, gy) = handle.growth();
    let min = f64::from(MIN_SIZE);
    // The size the pointer asks for, each axis on its own.
    let mut w = if gx == 0.0 { w0 } else { w0 + dx * gx };
    let mut h = if gy == 0.0 { h0 } else { h0 + dy * gy };
    let mut guides = Vec::new();
    // The moving edges snap: the left edge sits at x0 + w0 − w, the right
    // at x0 + w (likewise for y).
    let edge_x = |w: f64| if gx < 0.0 { x0 + w0 - w } else { x0 + w };
    let edge_y = |h: f64| if gy < 0.0 { y0 + h0 - h } else { y0 + h };
    let snap_x = |w: f64| -> Option<(f64, Guide)> {
        let snap = snap?;
        let (shift, guide) = snap_axis(Axis::X, snap.width, &[edge_x(w)], snap.threshold_x)?;
        // Moving the left edge right shrinks the box.
        Some((if gx < 0.0 { -shift } else { shift }, guide))
    };
    let snap_y = |h: f64| -> Option<(f64, Guide)> {
        let snap = snap?;
        let (shift, guide) = snap_axis(Axis::Y, snap.height, &[edge_y(h)], snap.threshold_y)?;
        Some((if gy < 0.0 { -shift } else { shift }, guide))
    };
    if handle.is_corner() && !free_aspect {
        // The aspect at the start: the larger pull wins, at least 16×16.
        let scale = ((w / w0).max(h / h0)).max(min / w0).max(min / h0);
        w = w0 * scale;
        h = h0 * scale;
        // A snapped edge sets the width; the height follows it below.
        if let Some((grow, guide)) =
            snap_x(w).filter(|(g, _)| w + g >= min && (w + g) * h0 / w0 >= min)
        {
            w += grow;
            guides.push(guide);
        } else if let Some((grow, guide)) =
            snap_y(h).filter(|(g, _)| h + g >= min && (h + g) * w0 / h0 >= min)
        {
            w = (h + grow) * w0 / h0;
            guides.push(guide);
        }
        // Rounded so w/h stays within a pixel of the start's.
        let w_px = round_size(w);
        let h_px = round_size(f64::from(w_px) * h0 / w0).max(MIN_SIZE);
        let (w, h) = (f64::from(w_px), f64::from(h_px));
        let x = if gx < 0.0 { x0 + w0 - w } else { x0 };
        let y = if gy < 0.0 { y0 + h0 - h } else { y0 };
        return DragFrame {
            rect: crate::model::Rect::new(round_i32(x), round_i32(y), w_px, h_px),
            crop: start.crop,
            guides,
        };
    }
    w = w.max(min);
    h = h.max(min);
    if gx != 0.0
        && let Some((grow, guide)) = snap_x(w).filter(|(g, _)| w + g >= min)
    {
        w += grow;
        guides.push(guide);
    }
    if gy != 0.0
        && let Some((grow, guide)) = snap_y(h).filter(|(g, _)| h + g >= min)
    {
        h += grow;
        guides.push(guide);
    }
    let (w_px, h_px) = (round_size(w), round_size(h));
    let x = if gx < 0.0 {
        x0 + w0 - f64::from(w_px)
    } else {
        x0
    };
    let y = if gy < 0.0 {
        y0 + h0 - f64::from(h_px)
    } else {
        y0
    };
    DragFrame {
        rect: crate::model::Rect::new(round_i32(x), round_i32(y), w_px, h_px),
        crop: start.crop,
        guides,
    }
}

/// `Alt` + an edge: that side's crop grows by the pointer's travel into the
/// box × source px per canvas px, leaving at least [`MIN_CROP_SOURCE`].
fn drag_crop(start: &DragStart, edge: Edge, dx: f64, dy: f64) -> Crop {
    let mut crop = start.crop;
    let Some((src_w, src_h)) = start.source else {
        return crop;
    };
    let Some(p) = place(src_w, src_h, start.crop, start.rect, start.fit) else {
        return crop;
    };
    let per_x = f64::from(p.src.w) / f64::from(p.dst.w.max(1));
    let per_y = f64::from(p.src.h) / f64::from(p.dst.h.max(1));
    let side = |current: u32, opposite: u32, total: u32, delta: f64| -> u32 {
        let most = total
            .saturating_sub(opposite)
            .saturating_sub(MIN_CROP_SOURCE);
        let wanted = (f64::from(current) + delta).round().max(0.0);
        (wanted.min(f64::from(most)) as u32).min(most)
    };
    match edge {
        Edge::Left => crop.left = side(crop.left, crop.right, src_w, dx * per_x),
        Edge::Right => crop.right = side(crop.right, crop.left, src_w, -dx * per_x),
        Edge::Top => crop.top = side(crop.top, crop.bottom, src_h, dy * per_y),
        Edge::Bottom => crop.bottom = side(crop.bottom, crop.top, src_h, -dy * per_y),
    }
    crop
}

/// A nudge burst's rect (D-33): `(dx, dy)` canvas px, ×10 with `shift`.
pub fn nudge(rect: crate::model::Rect, dx: i32, dy: i32, shift: bool) -> crate::model::Rect {
    let k = if shift { 10 } else { 1 };
    crate::model::Rect {
        x: rect.x.saturating_add(dx.saturating_mul(k)),
        y: rect.y.saturating_add(dy.saturating_mul(k)),
        ..rect
    }
}

/// The step of a `layer.nudge-*` binding: (dx, dy) of one press and
/// whether it is the ×10 one; `None` for other ids.
pub fn nudge_step(binding: &str) -> Option<(i32, i32, bool)> {
    let rest = binding.strip_prefix("layer.nudge-")?;
    let (dir, ten) = match rest.strip_suffix("-10") {
        Some(dir) => (dir, true),
        None => (rest, false),
    };
    let (dx, dy) = match dir {
        "left" => (-1, 0),
        "right" => (1, 0),
        "up" => (0, -1),
        "down" => (0, 1),
        _ => return None,
    };
    Some((dx, dy, ten))
}

/// A number as the readout and inspector show it: the true minus sign.
fn signed(v: i64) -> String {
    if v < 0 {
        format!("\u{2212}{}", v.unsigned_abs())
    } else {
        v.to_string()
    }
}

/// The position readout: "x 1280 · y 760 · 384 × 288".
pub fn readout(rect: crate::model::Rect) -> String {
    format!(
        "x {} \u{00b7} y {} \u{00b7} {} \u{00d7} {}",
        signed(i64::from(rect.x)),
        signed(i64::from(rect.y)),
        rect.w,
        rect.h
    )
}

/// How far the readout sits from the box.
pub const READOUT_GAP: f64 = 8.0;

/// Where the readout of `size` goes for a box at `box_rect` (preview-local):
/// 8 px above it, below it when there is no room above.
pub fn readout_origin(box_rect: Rect, size: (f64, f64)) -> (f64, f64) {
    let above = box_rect.y - READOUT_GAP - size.1;
    let y = if above >= 0.0 {
        above
    } else {
        box_rect.y + box_rect.h + READOUT_GAP
    };
    (box_rect.x, y)
}

// ------------------------------------------------------------- overlays

/// The countdown overlay (C07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Countdown {
    pub digit: u32,
    /// The ring's sweep, 0..1 over the second.
    pub sweep: f64,
    /// The take about to start.
    pub take: TakeId,
}

/// The number the next take gets: `nextTakeNumber`, past every take in
/// the pool (`ctx::next_take_number` without the disk's facts).
pub fn next_take(project: &Project) -> TakeId {
    let in_pool = project
        .takes
        .keys()
        .map(|t| t.number().saturating_add(1))
        .max()
        .unwrap_or(1);
    TakeId::numbered(project.next_take_number.max(in_pool).max(1))
}

/// The countdown, `ms_since_tick` into its current second: the digit, and
/// the sweep that empties over the second (full at the tick).
pub fn countdown(state: &State, ms_since_tick: u64) -> Option<Countdown> {
    let RecordPhase::Countdown { remaining, .. } = &state.runtime.record else {
        return None;
    };
    let sweep = 1.0 - (ms_since_tick.min(1000) as f64 / 1000.0);
    Some(Countdown {
        digit: *remaining,
        sweep,
        take: next_take(&state.project),
    })
}

/// The countdown plate's lines (C07): "Take 10", "Esc or R cancels".
pub fn countdown_plate(take: &TakeId, cancel_keys: &str) -> (String, String) {
    (
        format!("Take {}", take.number()),
        format!("{cancel_keys} cancels"),
    )
}

/// The REC cluster (C06).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RecBadge {
    #[default]
    None,
    /// "Starting take 10…" (D-04).
    Starting {
        take: TakeId,
    },
    Recording {
        take: TakeId,
        elapsed_ns: u64,
    },
    Saving {
        take: TakeId,
    },
}

/// The REC cluster of `state`.
pub fn rec(state: &State) -> RecBadge {
    match &state.runtime.record {
        RecordPhase::Starting { take, .. } => RecBadge::Starting { take: take.clone() },
        RecordPhase::Recording {
            take, elapsed_ns, ..
        } => RecBadge::Recording {
            take: take.clone(),
            elapsed_ns: *elapsed_ns,
        },
        RecordPhase::Finalizing { take, .. } => RecBadge::Saving { take: take.clone() },
        RecordPhase::Idle | RecordPhase::Countdown { .. } => RecBadge::None,
    }
}

impl RecBadge {
    /// The pill's text without a timer: "Starting take 10…" (D-04) or
    /// "Saving take 10…" (S05); `None` for the REC badge and nothing.
    pub fn pill(&self) -> Option<String> {
        match self {
            RecBadge::Starting { take } => Some(format!("Starting take {}\u{2026}", take.number())),
            RecBadge::Saving { take } => Some(format!("Saving take {}\u{2026}", take.number())),
            RecBadge::None | RecBadge::Recording { .. } => None,
        }
    }
}

/// The REC timer: the elapsed time as a timecode ("00:02:41:12").
pub fn rec_timer(elapsed_ns: u64, canvas: &Canvas) -> String {
    let ns = i64::try_from(elapsed_ns).unwrap_or(i64::MAX);
    format::timecode(
        ns_to_frame_floor(ns, canvas.fps).max(0) as Frame,
        canvas.fps,
    )
}

/// The transport row's left side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransportLeft {
    /// Program: "00:01:21:07" / "00:04:06:11".
    Timecode { now: String, total: String },
    /// Live: "Live · 1920×1080 · 30 fps".
    Live { text: String },
}

/// The transport row (C06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transport {
    pub left: TransportLeft,
    pub enabled: bool,
    /// The shuttle speed after the total: "· 2×", "· −4×" (D-26).
    pub rate: Option<String>,
}

/// The transport's tooltip for its buttons in Live mode (BRIEF §2.4).
pub const TRANSPORT_LIVE_TOOLTIP: &str = "Switch to Program (Tab) to play the timeline";

/// The transport row: the player's frame while playing. Live mode reads
/// "Live · <canvas>" with the buttons disabled; Program shows the playhead
/// (the player's frame while playing) over the total, and the shuttle
/// speed while it isn't 1×.
pub fn transport(state: &State, player_frame: Frame, playing: bool, rate: f64) -> Transport {
    let canvas = &state.project.canvas;
    match state.ui.canvas_mode {
        CanvasMode::Live => Transport {
            left: TransportLeft::Live {
                text: format!("Live \u{00b7} {}", format::canvas(canvas)),
            },
            enabled: false,
            rate: None,
        },
        CanvasMode::Program => {
            let total = total_frames(&state.project.timeline.clips);
            let now = if playing {
                player_frame
            } else {
                state.ui.playhead
            };
            Transport {
                left: TransportLeft::Timecode {
                    now: format::timecode(now.min(total), canvas.fps),
                    total: format::timecode(total, canvas.fps),
                },
                enabled: total > 0,
                rate: (playing && rate != 1.0).then(|| shuttle_rate(rate)),
            }
        }
    }
}

/// "· 2×", "· −4×" (D-26: the true minus).
pub fn shuttle_rate(rate: f64) -> String {
    let magnitude = rate.abs();
    let number = if magnitude.fract() == 0.0 {
        format!("{magnitude:.0}")
    } else {
        format!("{magnitude}")
    };
    let sign = if rate < 0.0 { "\u{2212}" } else { "" };
    format!("\u{00b7} {sign}{number}\u{00d7}")
}

/// A stage badge (C06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Badge {
    Live,
    Program,
    /// The bin preview (D-17): `TAKE 4`.
    Take {
        take: TakeId,
    },
    /// A preview-size player (H-9).
    Proxy,
}

impl Badge {
    /// Its text (uppercase, `badge` type).
    pub fn label(&self) -> String {
        match self {
            Badge::Live => "LIVE".to_owned(),
            Badge::Program => "PROGRAM".to_owned(),
            Badge::Take { take } => format!("TAKE {}", take.number()),
            Badge::Proxy => "PROXY".to_owned(),
        }
    }
}

/// The badges, right to left: the mode (or `TAKE 4`), then `PROXY` while
/// the player draws (Program or a bin preview).
pub fn badges(state: &State, take_preview: Option<&TakeId>, proxy: bool) -> Vec<Badge> {
    let program = state.ui.canvas_mode == CanvasMode::Program;
    let mut badges = vec![match take_preview {
        Some(take) => Badge::Take { take: take.clone() },
        None if program => Badge::Program,
        None => Badge::Live,
    }];
    if proxy && (program || take_preview.is_some()) {
        badges.push(Badge::Proxy);
    }
    badges
}

/// The trim plate (S10): "Trim start" + " · take 5 · frame " + "41" (mono).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimPlate {
    pub title: String,
    pub detail: String,
    pub frame: String,
}

/// The S10 plate for a trim of the start edge (`start`) or the end, showing
/// `frame` of `take`.
pub fn trim_plate(start: bool, take: &TakeId, frame: Frame) -> TrimPlate {
    TrimPlate {
        title: if start { "Trim start" } else { "Trim end" }.to_owned(),
        detail: format!(" \u{00b7} take {} \u{00b7} frame ", take.number()),
        frame: frame.to_string(),
    }
}

/// D-05: the Live picture while a take started in Program runs (the engine
/// adds no preview branch mid-take, A§8.11).
pub const LIVE_AFTER_TAKE: &str = "Live preview starts after this take";

/// Whether any visible layer of the project scene is a video source the
/// live status reports `Live` (the live picture has something to show).
pub fn live_video(state: &State, live: &LiveSources) -> bool {
    state.project.scene.layers.iter().any(|layer| {
        layer.visible
            && live
                .get(source_name(&layer.source))
                .is_some_and(|v| v.state == LiveSourceState::Live)
    })
}

// ------------------------------------------------------------- first run

/// What a first-run row adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Screen,
    Camera,
    Mic,
    SystemAudio,
}

/// One source of the first-run list (S01a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    pub kind: SourceKind,
    /// The output, device or source name it adds.
    pub source: String,
    /// "Screen · DP-4 · GIGA-BYTE GS27QA", "Mic · Yeti Nano".
    pub label: String,
    /// "2560×1440".
    pub meta: Option<String>,
    pub checked: bool,
}

/// A monitor source's name for people: "Monitor of X" reads "X".
fn monitor_label(description: &str) -> &str {
    description
        .strip_prefix("Monitor of ")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(description)
}

/// "Kind · name" (just "Kind · source" without a description).
fn row_label(kind: &str, parts: &[&str]) -> String {
    let mut label = kind.to_owned();
    for part in parts.iter().filter(|p| !p.trim().is_empty()) {
        label.push_str(" \u{00b7} ");
        label.push_str(part);
    }
    label
}

/// Whether the project is in its first run (S01a, D-27): Live mode with an
/// empty scene. The stage shows the source list then (unless a bin preview
/// or a trim scrub has the picture), and the hint bar's state is FirstRun
/// (G§4.3 step 8): in Program an empty scene isn't one.
pub fn first_run(state: &State) -> bool {
    state.ui.canvas_mode == CanvasMode::Live && state.project.scene.is_empty()
}

/// The first-run rows: screens, cameras, mics, monitors; the first screen
/// and camera and the default mic and monitor checked (D-16; the first one
/// when the system names no default).
pub fn first_run_rows(
    screens: &[ScreenInfo],
    cameras: &[CameraInfo],
    mics: &[AudioDeviceInfo],
    monitors: &[AudioDeviceInfo],
    default_mic: Option<&str>,
    default_monitor: Option<&str>,
) -> Vec<SourceRow> {
    let mut rows = Vec::new();
    for (i, screen) in screens.iter().enumerate() {
        rows.push(SourceRow {
            kind: SourceKind::Screen,
            source: screen.output.clone(),
            label: row_label("Screen", &[&screen.output, &screen.description]),
            meta: Some(format!("{}\u{00d7}{}", screen.width, screen.height)),
            checked: i == 0,
        });
    }
    for (i, camera) in cameras.iter().enumerate() {
        rows.push(SourceRow {
            kind: SourceKind::Camera,
            source: camera.device.clone(),
            label: row_label("Camera", &[&camera.label]),
            meta: Some(camera.device.clone()),
            checked: i == 0,
        });
    }
    let audio = |kind: SourceKind,
                 devices: &[AudioDeviceInfo],
                 default: Option<&str>,
                 rows: &mut Vec<SourceRow>| {
        let default = default.filter(|d| devices.iter().any(|dev| dev.name == *d));
        for (i, device) in devices.iter().enumerate() {
            let (word, name) = match kind {
                SourceKind::Mic => ("Mic", device.description.as_str()),
                _ => ("System audio", monitor_label(&device.description)),
            };
            let name = if name.trim().is_empty() {
                device.name.as_str()
            } else {
                name
            };
            rows.push(SourceRow {
                kind,
                source: device.name.clone(),
                label: row_label(word, &[name]),
                meta: None,
                checked: match default {
                    Some(d) => device.name == d,
                    None => i == 0,
                },
            });
        }
    };
    audio(SourceKind::Mic, mics, default_mic, &mut rows);
    audio(
        SourceKind::SystemAudio,
        monitors,
        default_monitor,
        &mut rows,
    );
    rows
}

/// What the person checked or unchecked on the first-run list, by row
/// (kind, source).
pub type FirstRunChoices = BTreeMap<(SourceKind, String), bool>;

/// `rows` (from [`first_run_rows`]) with the person's `choices` applied: a
/// refreshed inventory (a device plugged in, an Add menu's discovery)
/// rebuilds the list, and the rows they toggled keep their state while the
/// others follow the defaults (R25: the rebuild reset every check, so
/// `Enter` added a camera they had unchecked).
pub fn with_choices(rows: Vec<SourceRow>, choices: &FirstRunChoices) -> Vec<SourceRow> {
    rows.into_iter()
        .map(|mut row| {
            if let Some(&checked) = choices.get(&(row.kind, row.source.clone())) {
                row.checked = checked;
            }
            row
        })
        .collect()
}

/// The commands "Add N sources" sends, in one FIFO: screens, cameras, the
/// mic, system audio (D-16), so the camera lands on top of the screens.
pub fn first_run_commands(rows: &[SourceRow]) -> Vec<Command> {
    let mut cmds = Vec::new();
    for kind in [
        SourceKind::Screen,
        SourceKind::Camera,
        SourceKind::Mic,
        SourceKind::SystemAudio,
    ] {
        for row in rows.iter().filter(|r| r.checked && r.kind == kind) {
            cmds.push(match kind {
                SourceKind::Screen => Command::LayerAdd {
                    target: SceneTarget::Project,
                    source: VideoSourceArg::Screen {
                        output: row.source.clone(),
                    },
                    name: None,
                    rect: None,
                },
                SourceKind::Camera => Command::LayerAdd {
                    target: SceneTarget::Project,
                    source: VideoSourceArg::Camera {
                        device: row.source.clone(),
                    },
                    name: None,
                    rect: None,
                },
                SourceKind::Mic | SourceKind::SystemAudio => Command::AudioAdd {
                    target: SceneTarget::Project,
                    role: if kind == SourceKind::Mic {
                        AudioRole::Mic
                    } else {
                        AudioRole::System
                    },
                    source: Some(row.source.clone()),
                    label: None,
                },
            });
        }
    }
    cmds
}

/// The first-run button: "Add 4 sources", "Add 1 source" ("Add sources"
/// with none checked: the button is disabled then).
pub fn add_sources_label(checked: usize) -> String {
    match checked {
        0 => "Add sources".to_owned(),
        1 => "Add 1 source".to_owned(),
        n => format!("Add {n} sources"),
    }
}

/// The first-run list's heading and caption (S01a).
pub const FIRST_RUN_HEADING: &str = "What should Qrec record?";
pub const FIRST_RUN_CAPTION: &str =
    "Screens ask for permission once. Your desktop shows a picker for each one.";

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::ids::ClipId;
    use crate::model::Rect as CanvasRect;
    use crate::state::RecordMode;
    use crate::testkit;
    use crate::view::LiveSourceView;

    fn rounded(r: Rect) -> [f64; 4] {
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        [round(r.x), round(r.y), round(r.w), round(r.h)]
    }

    #[test]
    fn edit_target_is_the_core_rule() {
        let mut state = testkit::design_state();
        assert_eq!(edit_target(&state), Some(SceneTarget::Project));
        state.ui.canvas_mode = CanvasMode::Program;
        assert_eq!(edit_target(&state), None, "S07: nothing selected");
        state.ui.selection = vec![ClipId::from_raw("c2d0")];
        assert_eq!(
            edit_target(&state),
            Some(SceneTarget::Take(TakeId::numbered(5)))
        );
    }

    /// G§4.6's checks: the default stage and the bin open; add the stage
    /// origin, (0, 48) or (352, 48), for window coordinates (S02, S14).
    #[test]
    fn stage_geometry_matches_the_frames() {
        let canvas = Canvas::default();
        let s02 = stage_geometry(1216.0, 608.0, &canvas);
        assert_eq!(rounded(s02.preview), [128.0, 16.0, 960.0, 540.0]);
        assert_eq!(rounded(s02.transport), [128.0, 564.0, 960.0, 36.0]);
        let s14 = stage_geometry(864.0, 608.0, &canvas);
        assert_eq!(rounded(s14.preview), [16.0, 52.0, 832.0, 468.0]);
        assert_eq!(rounded(s14.transport), [16.0, 564.0, 832.0, 36.0]);
        // A compact window (1600×800): the stage shrinks, the row stays.
        let compact = stage_geometry(1216.0, 488.0, &canvas);
        assert_eq!(compact.transport.y, 444.0);
        assert!((compact.preview.w / compact.preview.h - 16.0 / 9.0).abs() < 1e-9);
        insta::assert_json_snapshot!(
            "stage_geometry",
            json!({"s02": s02, "s14": s14, "compact": compact,
                   "narrow": stage_geometry(1000.0, 608.0, &canvas),
                   "vertical": stage_geometry(1216.0, 608.0, &Canvas { width: 1080, height: 1920, ..Canvas::default() })})
        );
    }

    fn design_sizes(state: &State) -> BTreeMap<LayerId, (u32, u32)> {
        // The live status of the design scene's sources (BRIEF §3.1).
        let live: LiveSources = [("DP-4", (2560, 1440)), ("/dev/video0", (1280, 720))]
            .into_iter()
            .map(|(name, size)| {
                (
                    name.to_owned(),
                    LiveSourceView {
                        state: LiveSourceState::Live,
                        size: Some(size),
                    },
                )
            })
            .collect();
        source_sizes(&state.project, &SceneTarget::Project, &live)
    }

    /// G§4.6: L2 at rest (1500, 760, 384, 288) is at preview-local (750,
    /// 380, 192, 144); dragged to x 1280 (S13) at (640, 380, 192, 144).
    #[test]
    fn the_design_scenes_layer_boxes() {
        let state = testkit::design_state();
        let canvas = &state.project.canvas;
        let preview = stage_geometry(1216.0, 608.0, canvas).preview;
        let sizes = design_sizes(&state);
        let boxes = layer_boxes(&state.project.scene, canvas, &sizes, preview);
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].layer.as_str(), "L2");
        assert_eq!(rounded(boxes[1].rect), [750.0, 380.0, 192.0, 144.0]);
        assert_eq!(rounded(boxes[0].rect), [0.0, 0.0, 960.0, 540.0]);
        let mut scene = state.project.scene.clone();
        scene.layers[1].rect.x = 1280;
        let moved = layer_boxes(&scene, canvas, &sizes, preview);
        assert_eq!(rounded(moved[1].rect), [640.0, 380.0, 192.0, 144.0]);
        // Unknown sizes: the rect.
        let unknown = layer_boxes(&state.project.scene, canvas, &BTreeMap::new(), preview);
        assert_eq!(rounded(unknown[1].rect), [750.0, 380.0, 192.0, 144.0]);
        // Program: the take's streams (take 5's camera is 1280×720).
        let take = SceneTarget::Take(TakeId::numbered(5));
        let take_sizes = source_sizes(&state.project, &take, &LiveSources::new());
        assert_eq!(take_sizes.get(&"L2".parse().unwrap()), Some(&(1280, 720)));
        assert_eq!(take_sizes.get(&"L1".parse().unwrap()), Some(&(2560, 1440)));
        insta::assert_json_snapshot!("design_layer_boxes", json!({"live": boxes, "moved": moved}));
    }

    /// R20: a Contain layer whose rect's aspect isn't its source's (a
    /// 16:10 screen on the 16:9 canvas: screens' default fit) is
    /// pillarboxed. The handles sit on the rect, which a resize edits, so
    /// a grabbed handle follows the pointer; the body is the picture.
    #[test]
    fn contain_handles_sit_on_the_rect_they_edit() {
        let mut state = testkit::design_state();
        state.project.scene.layers.truncate(1);
        let canvas = state.project.canvas.clone();
        let preview = Rect::new(0.0, 0.0, 960.0, 540.0);
        let l1: LayerId = "L1".parse().unwrap();
        let sizes: BTreeMap<LayerId, (u32, u32)> = [(l1.clone(), (2560, 1600))].into();
        let boxes = layer_boxes(&state.project.scene, &canvas, &sizes, preview);
        assert_eq!(
            rounded(boxes[0].rect),
            [48.0, 0.0, 864.0, 540.0],
            "the picture"
        );
        assert_eq!(
            rounded(boxes[0].frame),
            [0.0, 0.0, 960.0, 540.0],
            "the rect"
        );
        // The left handle is on the rect's left edge; the pillarbox isn't
        // the body.
        assert_eq!(
            hit(&boxes, Some(&l1), 2.0, 270.0),
            Hit::Handle {
                layer: l1.clone(),
                handle: Handle::Left
            }
        );
        assert_eq!(hit(&boxes, None, 20.0, 270.0), Hit::None);
        // Dragged 50 preview px (100 canvas px) left, the handle is 50 px
        // further left too: under the pointer.
        let layer = state.project.scene.layers[0].clone();
        let frame = drag(
            &DragStart::of(&layer, Some((2560, 1600))),
            DragOp::Resize {
                handle: Handle::Left,
            },
            -100.0,
            0.0,
            false,
            None,
        );
        let mut scene = state.project.scene.clone();
        scene.layers[0].rect = frame.rect;
        let moved = layer_boxes(&scene, &canvas, &sizes, preview);
        assert_eq!(Handle::Left.center(moved[0].frame).0.round(), -50.0);
    }

    #[test]
    fn hits_take_the_handles_first_then_the_topmost_box() {
        let state = testkit::design_state();
        let canvas = &state.project.canvas;
        let preview = stage_geometry(1216.0, 608.0, canvas).preview;
        let boxes = layer_boxes(&state.project.scene, canvas, &design_sizes(&state), preview);
        let l1: LayerId = "L1".parse().unwrap();
        let l2: LayerId = "L2".parse().unwrap();
        // The face cam is on top of the screen.
        assert_eq!(
            hit(&boxes, None, 800.0, 400.0),
            Hit::Body { layer: l2.clone() }
        );
        assert_eq!(
            hit(&boxes, None, 100.0, 100.0),
            Hit::Body { layer: l1.clone() }
        );
        assert_eq!(hit(&boxes, None, 970.0, 100.0), Hit::None, "off the canvas");
        // The selected box's handles grab 12 px squares around them.
        assert_eq!(
            hit(&boxes, Some(&l2), 750.0 + 5.0, 380.0 - 5.0),
            Hit::Handle {
                layer: l2.clone(),
                handle: Handle::TopLeft
            }
        );
        assert_eq!(
            hit(&boxes, Some(&l2), 942.0, 452.0),
            Hit::Handle {
                layer: l2.clone(),
                handle: Handle::Right
            }
        );
        assert_eq!(
            hit(&boxes, Some(&l2), 940.0 + 4.0, 520.0 + 4.0),
            Hit::Handle {
                layer: l2.clone(),
                handle: Handle::BottomRight
            }
        );
        // Without the selection there are no handles: the body below (the
        // box's right edge, x 942, is outside it).
        assert_eq!(
            hit(&boxes, None, 941.0, 452.0),
            Hit::Body { layer: l2.clone() }
        );
        assert_eq!(
            hit(&boxes, None, 942.0, 452.0),
            Hit::Body { layer: l1.clone() }
        );
        // Hidden layers can't be hit.
        let mut scene = state.project.scene.clone();
        scene.layers[1].visible = false;
        let hidden = layer_boxes(&scene, canvas, &design_sizes(&state), preview);
        assert_eq!(
            hit(&hidden, Some(&l2), 800.0, 400.0),
            Hit::Body { layer: l1 }
        );
        assert_eq!(
            DragOp::for_handle(Handle::Left, true),
            DragOp::Crop { edge: Edge::Left }
        );
        assert_eq!(
            DragOp::for_handle(Handle::TopLeft, true),
            DragOp::Resize {
                handle: Handle::TopLeft
            }
        );
        assert_eq!(Handle::TopRight.cursor(), "nesw-resize");
    }

    fn l2_start(state: &State) -> DragStart {
        DragStart::of(&state.project.scene.layers[1], Some((1280, 720)))
    }

    fn design_snap(state: &State) -> Snap {
        let canvas = &state.project.canvas;
        Snap::new(canvas, Scale::of(canvas, Rect::new(0.0, 0.0, 960.0, 540.0)))
    }

    /// S13: L2's left edge dragged near x 1280 snaps to the canvas' right
    /// third; snapping off leaves it where the pointer put it.
    #[test]
    fn a_move_snaps_at_right_third() {
        let state = testkit::design_state();
        let start = l2_start(&state);
        let snap = design_snap(&state);
        assert_eq!(snap.threshold_x, 16.0, "8 preview px at 1:2");
        let frame = drag(&start, DragOp::Move, -227.0, 3.0, false, Some(&snap));
        assert_eq!(frame.rect, CanvasRect::new(1280, 763, 384, 288));
        assert_eq!(frame.guides.len(), 1);
        assert_eq!(frame.guides[0].label, "Right third");
        assert_eq!(frame.guides[0].at, 1280.0);
        assert_eq!(
            readout(frame.rect),
            "x 1280 \u{b7} y 763 \u{b7} 384 \u{d7} 288"
        );
        let free = drag(&start, DragOp::Move, -227.0, 3.0, false, None);
        assert_eq!(free.rect, CanvasRect::new(1273, 763, 384, 288));
        assert!(free.guides.is_empty());
        // The bottom edge (760 + 288 = 1048) is 32 px from the canvas'
        // bottom: no snap; nudged down 25 it snaps (1073 → 1080).
        let low = drag(&start, DragOp::Move, 0.0, 25.0, false, Some(&snap));
        assert_eq!(low.rect.y, 792);
        assert_eq!(low.guides[0].label, "Bottom edge");
        insta::assert_json_snapshot!(
            "snap_right_third",
            json!({"snapped": frame, "free": free, "low": low})
        );
    }

    #[test]
    fn resizes_keep_the_aspect_unless_shift() {
        let state = testkit::design_state();
        let start = l2_start(&state);
        // The bottom-right corner pulled out by 96 px: 4:3 kept.
        let corner = drag(
            &start,
            DragOp::Resize {
                handle: Handle::BottomRight,
            },
            96.0,
            10.0,
            false,
            None,
        );
        assert_eq!(corner.rect, CanvasRect::new(1500, 760, 480, 360));
        // The top-left corner: the bottom-right corner stays put.
        let top_left = drag(
            &start,
            DragOp::Resize {
                handle: Handle::TopLeft,
            },
            -96.0,
            0.0,
            false,
            None,
        );
        assert_eq!(top_left.rect, CanvasRect::new(1404, 688, 480, 360));
        // Shift frees it.
        let free = drag(
            &start,
            DragOp::Resize {
                handle: Handle::BottomRight,
            },
            96.0,
            10.0,
            true,
            None,
        );
        assert_eq!(free.rect, CanvasRect::new(1500, 760, 480, 298));
        // An edge changes one axis; the left edge moves the origin.
        let left = drag(
            &start,
            DragOp::Resize {
                handle: Handle::Left,
            },
            40.0,
            99.0,
            false,
            None,
        );
        assert_eq!(left.rect, CanvasRect::new(1540, 760, 344, 288));
        // Never under 16×16.
        let tiny = drag(
            &start,
            DragOp::Resize {
                handle: Handle::BottomRight,
            },
            -5000.0,
            -5000.0,
            false,
            None,
        );
        assert!(
            tiny.rect.w >= MIN_SIZE && tiny.rect.h >= MIN_SIZE,
            "{tiny:?}"
        );
        // A snapped corner: the right edge to the canvas' right edge.
        let snapped = drag(
            &start,
            DragOp::Resize {
                handle: Handle::BottomRight,
            },
            30.0,
            0.0,
            false,
            Some(&design_snap(&state)),
        );
        assert_eq!(snapped.rect.x + snapped.rect.w as i32, 1920);
        assert_eq!(snapped.guides[0].label, "Right edge");
        insta::assert_json_snapshot!(
            "aspect_resize",
            json!({"corner": corner, "topLeft": top_left, "free": free, "left": left, "tiny": tiny, "snapped": snapped})
        );
    }

    /// `Alt` + an edge: the crop grows by the travel × source / dst and
    /// always leaves 16 source px.
    #[test]
    fn crops_stop_at_16_source_px() {
        let state = testkit::design_state();
        let start = l2_start(&state);
        // Cover: 960×720 of the 1280×720 camera fills 384×288, 2.5 source
        // px per canvas px.
        let left = drag(
            &start,
            DragOp::Crop { edge: Edge::Left },
            20.0,
            0.0,
            false,
            None,
        );
        assert_eq!(left.crop.left, 50);
        assert_eq!(left.rect, start.rect, "a crop keeps the rect");
        let right = drag(
            &start,
            DragOp::Crop { edge: Edge::Right },
            -8.0,
            0.0,
            false,
            None,
        );
        assert_eq!(right.crop.right, 20);
        let all = drag(
            &start,
            DragOp::Crop { edge: Edge::Left },
            9999.0,
            0.0,
            false,
            None,
        );
        assert_eq!(all.crop.left, 1280 - MIN_CROP_SOURCE);
        let back = drag(
            &start,
            DragOp::Crop { edge: Edge::Top },
            0.0,
            -50.0,
            false,
            None,
        );
        assert_eq!(back.crop.top, 0, "never negative");
        let bottom = drag(
            &start,
            DragOp::Crop { edge: Edge::Bottom },
            0.0,
            -4.0,
            false,
            None,
        );
        assert_eq!(bottom.crop.bottom, 10);
        // Without a known source size the crop can't move.
        let blind = drag(
            &DragStart {
                source: None,
                ..start.clone()
            },
            DragOp::Crop { edge: Edge::Left },
            20.0,
            0.0,
            false,
            None,
        );
        assert_eq!(blind.crop, start.crop);
        insta::assert_json_snapshot!(
            "crop_limits",
            json!({"left": left, "right": right, "all": all, "bottom": bottom})
        );
    }

    #[test]
    fn nudges_and_readouts() {
        let rect = CanvasRect::new(1500, 760, 384, 288);
        assert_eq!(
            nudge(rect, -1, 0, false),
            CanvasRect::new(1499, 760, 384, 288)
        );
        assert_eq!(
            nudge(rect, 0, 1, true),
            CanvasRect::new(1500, 770, 384, 288)
        );
        assert_eq!(nudge_step("layer.nudge-left"), Some((-1, 0, false)));
        assert_eq!(nudge_step("layer.nudge-down-10"), Some((0, 1, true)));
        assert_eq!(nudge_step("layer.nudge-sideways"), None);
        assert_eq!(nudge_step("clip.split"), None);
        assert_eq!(
            readout(CanvasRect::new(-12, 0, 16, 16)),
            "x \u{2212}12 \u{b7} y 0 \u{b7} 16 \u{d7} 16"
        );
        // S13: 8 px above the box; below it near the top edge.
        let size = (204.0, 24.0);
        assert_eq!(
            readout_origin(Rect::new(640.0, 380.0, 192.0, 144.0), size),
            (640.0, 348.0)
        );
        assert_eq!(
            readout_origin(Rect::new(10.0, 12.0, 100.0, 50.0), size),
            (10.0, 70.0)
        );
    }

    fn with_phase(phase: RecordPhase) -> State {
        let mut state = testkit::design_state();
        state.runtime.record = phase;
        state
    }

    #[test]
    fn overlays_follow_the_record_phase() {
        let take = TakeId::numbered(10);
        let countdown_state = with_phase(RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: crate::ids::TimerId(7),
            start_req: None,
            wait: false,
        });
        let c = countdown(&countdown_state, 0).expect("counting down");
        assert_eq!((c.digit, c.sweep, c.take.clone()), (3, 1.0, take.clone()));
        assert_eq!(countdown(&countdown_state, 250).unwrap().sweep, 0.75);
        assert_eq!(countdown(&countdown_state, 5000).unwrap().sweep, 0.0);
        assert_eq!(countdown(&testkit::design_state(), 0), None);
        assert_eq!(
            countdown_plate(&c.take, "Esc or R"),
            ("Take 10".to_owned(), "Esc or R cancels".to_owned())
        );
        let phases = [
            RecordPhase::Starting {
                take: take.clone(),
                mode: RecordMode::Append,
                start_req: None,
                wait: false,
            },
            RecordPhase::Recording {
                take: take.clone(),
                mode: RecordMode::Append,
                elapsed_ns: 161_400_000_000,
                waiters: vec![],
            },
            RecordPhase::Finalizing {
                take: take.clone(),
                mode: RecordMode::Append,
                cancelled: false,
                stop_reqs: vec![],
                waiters: vec![],
                discard: None,
            },
        ];
        let badges: Vec<RecBadge> = phases.iter().map(|p| rec(&with_phase(p.clone()))).collect();
        assert_eq!(
            badges[0].pill().as_deref(),
            Some("Starting take 10\u{2026}")
        );
        assert_eq!(badges[1].pill(), None);
        assert_eq!(badges[2].pill().as_deref(), Some("Saving take 10\u{2026}"));
        assert_eq!(rec(&testkit::design_state()), RecBadge::None);
        // S04: 4,842 frames at 30 fps.
        assert_eq!(
            rec_timer(161_400_000_000, &Canvas::default()),
            "00:02:41:12"
        );
        insta::assert_json_snapshot!("record_overlays", json!({"countdown": c, "badges": badges}));
    }

    #[test]
    fn the_transport_and_the_badges() {
        let live = testkit::design_state();
        let mut program = live.clone();
        program.ui.canvas_mode = CanvasMode::Program;
        let mut empty = program.clone();
        empty.project.timeline.clips.clear();
        let rows = json!({
            "live": transport(&live, 0, false, 1.0),
            "program": transport(&program, 9999, false, 1.0),
            "playing": transport(&program, 2500, true, 1.0),
            "shuttle": transport(&program, 2500, true, 2.0),
            "reverse": transport(&program, 2500, true, -4.0),
            "empty": transport(&empty, 0, false, 1.0),
        });
        assert_eq!(
            transport(&live, 0, false, 1.0).left,
            TransportLeft::Live {
                text: "Live \u{b7} 1920\u{d7}1080 \u{b7} 30 fps".into()
            }
        );
        assert_eq!(
            transport(&program, 0, false, 1.0).left,
            TransportLeft::Timecode {
                now: "00:01:21:07".into(),
                total: "00:04:06:11".into()
            }
        );
        assert_eq!(
            transport(&program, 2500, true, -4.0).rate.as_deref(),
            Some("\u{b7} \u{2212}4\u{d7}")
        );
        assert!(!transport(&empty, 0, false, 1.0).enabled);
        let take4 = TakeId::numbered(4);
        assert_eq!(badges(&live, None, false), [Badge::Live]);
        assert_eq!(badges(&live, None, true), [Badge::Live], "no proxy in Live");
        assert_eq!(badges(&program, None, true), [Badge::Program, Badge::Proxy]);
        assert_eq!(
            badges(&live, Some(&take4), false),
            [Badge::Take {
                take: take4.clone()
            }]
        );
        assert_eq!(Badge::Take { take: take4 }.label(), "TAKE 4");
        assert_eq!(
            trim_plate(true, &TakeId::numbered(5), 41),
            TrimPlate {
                title: "Trim start".into(),
                detail: " \u{b7} take 5 \u{b7} frame ".into(),
                frame: "41".into()
            }
        );
        insta::assert_json_snapshot!("transport", rows);
    }

    struct Inventory {
        screens: Vec<ScreenInfo>,
        cameras: Vec<CameraInfo>,
        mics: Vec<AudioDeviceInfo>,
        monitors: Vec<AudioDeviceInfo>,
    }

    fn design_inventory() -> Inventory {
        let screen = |output: &str, description: &str, width, height| ScreenInfo {
            output: output.into(),
            description: description.into(),
            x: 0,
            y: 0,
            width,
            height,
            scale: 1.0,
        };
        Inventory {
            screens: vec![
                screen("DP-4", "GIGA-BYTE GS27QA", 2560, 1440),
                screen("HDMI-A-1", "Dell U2720Q", 3840, 2160),
            ],
            cameras: vec![CameraInfo {
                device: "/dev/video0".into(),
                label: "Logitech C920".into(),
                modes: vec![],
            }],
            mics: vec![AudioDeviceInfo {
                name: "alsa_input.usb-Blue_Microphones_Yeti_Nano".into(),
                description: "Yeti Nano".into(),
                is_monitor: false,
            }],
            monitors: vec![AudioDeviceInfo {
                name: "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor".into(),
                description: "Monitor of Built-in Audio Analog Stereo".into(),
                is_monitor: true,
            }],
        }
    }

    /// S01a: BRIEF's five rows, four checked, and D-16's command order.
    #[test]
    fn the_first_run_rows_and_commands() {
        let Inventory {
            screens,
            cameras,
            mics,
            monitors,
        } = design_inventory();
        let rows = first_run_rows(
            &screens,
            &cameras,
            &mics,
            &monitors,
            Some("alsa_input.usb-Blue_Microphones_Yeti_Nano"),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"),
        );
        let labels: Vec<(&str, Option<&str>, bool)> = rows
            .iter()
            .map(|r| (r.label.as_str(), r.meta.as_deref(), r.checked))
            .collect();
        assert_eq!(
            labels,
            [
                (
                    "Screen \u{b7} DP-4 \u{b7} GIGA-BYTE GS27QA",
                    Some("2560\u{d7}1440"),
                    true
                ),
                (
                    "Screen \u{b7} HDMI-A-1 \u{b7} Dell U2720Q",
                    Some("3840\u{d7}2160"),
                    false
                ),
                ("Camera \u{b7} Logitech C920", Some("/dev/video0"), true),
                ("Mic \u{b7} Yeti Nano", None, true),
                (
                    "System audio \u{b7} Built-in Audio Analog Stereo",
                    None,
                    true
                ),
            ]
        );
        let checked = rows.iter().filter(|r| r.checked).count();
        assert_eq!(add_sources_label(checked), "Add 4 sources");
        assert_eq!(add_sources_label(1), "Add 1 source");
        assert_eq!(add_sources_label(0), "Add sources");
        let cmds = first_run_commands(&rows);
        let names: Vec<&str> = cmds.iter().map(Command::name).collect();
        assert_eq!(names, ["layerAdd", "layerAdd", "audioAdd", "audioAdd"]);
        // The camera goes in after the screens, so it lands on top.
        assert!(matches!(
            &cmds[1],
            Command::LayerAdd { source: VideoSourceArg::Camera { device }, .. } if device == "/dev/video0"
        ));
        // Without a default (or with one that isn't there) the first
        // device is checked.
        let no_defaults = first_run_rows(&screens, &cameras, &mics, &monitors, None, Some("gone"));
        let checked: Vec<SourceKind> = no_defaults
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.kind)
            .collect();
        assert_eq!(
            checked,
            [
                SourceKind::Screen,
                SourceKind::Camera,
                SourceKind::Mic,
                SourceKind::SystemAudio
            ]
        );
        insta::assert_json_snapshot!("first_run", json!({"rows": rows, "commands": cmds}));
    }

    /// D-27, R80: the first run is Live mode's with an empty scene; in
    /// Program, or with a layer or a track, there is none (the stage and the
    /// hint bar read this one rule).
    #[test]
    fn the_first_run_is_live_with_an_empty_scene() {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Live;
        assert!(!first_run(&state), "the design scene has layers");
        state.project.scene = Scene::default();
        assert!(first_run(&state));
        state.ui.canvas_mode = CanvasMode::Program;
        assert!(!first_run(&state));
    }

    /// R25: an inventory refresh (a mic plugged in, an Add menu's
    /// discovery) rebuilds the rows: the person's own picks stay, rows they
    /// never touched follow the defaults (a new default mic would replace
    /// the old one).
    #[test]
    fn first_run_rows_keep_the_persons_choices() {
        let Inventory {
            screens,
            cameras,
            mut mics,
            monitors,
        } = design_inventory();
        let yeti = "alsa_input.usb-Blue_Microphones_Yeti_Nano";
        let monitor = "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor";
        // The person unchecks the camera ("Add 3 sources").
        let mut choices = FirstRunChoices::new();
        choices.insert((SourceKind::Camera, "/dev/video0".to_owned()), false);
        // A USB mic comes, not the default.
        mics.push(AudioDeviceInfo {
            name: "alsa_input.usb-Samson_Q2U".into(),
            description: "Samson Q2U".into(),
            is_monitor: false,
        });
        let rows = with_choices(
            first_run_rows(
                &screens,
                &cameras,
                &mics,
                &monitors,
                Some(yeti),
                Some(monitor),
            ),
            &choices,
        );
        let checked: Vec<(SourceKind, &str)> = rows
            .iter()
            .filter(|r| r.checked)
            .map(|r| (r.kind, r.source.as_str()))
            .collect();
        assert_eq!(
            checked,
            [
                (SourceKind::Screen, "DP-4"),
                (SourceKind::Mic, yeti),
                (SourceKind::SystemAudio, monitor)
            ]
        );
        assert_eq!(first_run_commands(&rows).len(), 3, "no camera");
        // Ticking the new mic too.
        choices.insert(
            (SourceKind::Mic, "alsa_input.usb-Samson_Q2U".to_owned()),
            true,
        );
        let rows = with_choices(
            first_run_rows(
                &screens,
                &cameras,
                &mics,
                &monitors,
                Some(yeti),
                Some(monitor),
            ),
            &choices,
        );
        assert_eq!(rows.iter().filter(|r| r.checked).count(), 4);
    }

    #[test]
    fn live_video_needs_a_visible_live_source() {
        let state = testkit::design_state();
        let mut live = LiveSources::new();
        assert!(!live_video(&state, &live));
        live.insert(
            "/dev/video0".into(),
            LiveSourceView {
                state: LiveSourceState::AwaitingPortal,
                size: None,
            },
        );
        assert!(!live_video(&state, &live));
        live.get_mut("/dev/video0").unwrap().state = LiveSourceState::Live;
        assert!(live_video(&state, &live));
        assert_eq!(next_take(&state.project), TakeId::numbered(10));
    }

    fn canvas_strategy() -> impl Strategy<Value = Canvas> {
        (16u32..4000, 16u32..4000).prop_map(|(width, height)| Canvas {
            width,
            height,
            ..Canvas::default()
        })
    }

    fn rect_strategy() -> impl Strategy<Value = CanvasRect> {
        (-500i32..2500, -500i32..1500, 16u32..2000, 16u32..1200)
            .prop_map(|(x, y, w, h)| CanvasRect::new(x, y, w, h))
    }

    proptest! {
        /// The preview always fits the stage at the canvas aspect.
        #[test]
        fn the_preview_fits_the_stage(w in 40.0f64..4000.0, h in 80.0f64..3000.0, canvas in canvas_strategy()) {
            let g = stage_geometry(w, h, &canvas);
            let p = g.preview;
            prop_assert!(p.x >= STAGE_PAD - 1e-9 && p.x + p.w <= w - STAGE_PAD + 1e-9);
            prop_assert!(p.y >= STAGE_PAD - 1e-9 && p.y + p.h <= h - BELOW_PREVIEW + 1e-9);
            let aspect = f64::from(canvas.width) / f64::from(canvas.height);
            prop_assert!((p.w - p.h * aspect).abs() < 1e-6 * p.w.max(1.0));
            prop_assert!(p.w + 1e-6 >= w - 2.0 * STAGE_PAD || p.h + 1e-6 >= h - STAGE_PAD - BELOW_PREVIEW);
            prop_assert_eq!(g.transport.y, h - TRANSPORT_BOTTOM);
        }

        /// Snapping never moves a feature more than the threshold.
        #[test]
        fn snapping_stays_within_the_threshold(
            start in rect_strategy(),
            dx in -800.0f64..800.0,
            dy in -800.0f64..800.0,
            canvas in canvas_strategy(),
        ) {
            let snap = Snap::new(&canvas, Scale::of(&canvas, Rect::new(0.0, 0.0, f64::from(canvas.width) / 2.0, f64::from(canvas.height) / 2.0)));
            let ds = DragStart { rect: start, crop: Crop::default(), fit: Fit::Contain, source: None };
            let snapped = drag(&ds, DragOp::Move, dx, dy, false, Some(&snap));
            let free = drag(&ds, DragOp::Move, dx, dy, false, None);
            // Each axis moves at most the threshold (and a rounding px).
            prop_assert!(f64::from((snapped.rect.x - free.rect.x).abs()) <= snap.threshold_x + 1.0);
            prop_assert!(f64::from((snapped.rect.y - free.rect.y).abs()) <= snap.threshold_y + 1.0);
            prop_assert_eq!((snapped.rect.w, snapped.rect.h), (start.w, start.h));
        }

        /// An aspect-locked resize keeps w/h within a pixel.
        #[test]
        fn aspect_locked_resizes_keep_the_aspect(
            start in rect_strategy(),
            corner in 0usize..4,
            dx in -3000.0f64..3000.0,
            dy in -3000.0f64..3000.0,
            snap_on: bool,
            canvas in canvas_strategy(),
        ) {
            let handle = [Handle::TopLeft, Handle::TopRight, Handle::BottomRight, Handle::BottomLeft][corner];
            let snap = Snap::new(&canvas, Scale::of(&canvas, Rect::new(0.0, 0.0, 960.0, 540.0)));
            let ds = DragStart { rect: start, crop: Crop::default(), fit: Fit::Cover, source: None };
            let frame = drag(&ds, DragOp::Resize { handle }, dx, dy, false, snap_on.then_some(&snap));
            let (w, h) = (f64::from(frame.rect.w), f64::from(frame.rect.h));
            prop_assert!(frame.rect.w >= MIN_SIZE && frame.rect.h >= MIN_SIZE);
            let want_h = w * f64::from(start.h) / f64::from(start.w);
            // Within a pixel, unless the 16 px floor of the short side bit.
            prop_assert!((h - want_h).abs() <= 1.0 || frame.rect.h == MIN_SIZE, "{:?} from {:?}", frame.rect, start);
        }

        /// A crop never leaves fewer than 16 source px.
        #[test]
        fn crops_leave_16_source_px(
            start in rect_strategy(),
            sw in 32u32..4000,
            sh in 32u32..3000,
            edge in 0usize..4,
            dx in -5000.0f64..5000.0,
            dy in -5000.0f64..5000.0,
            cover: bool,
        ) {
            let edge = [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left][edge];
            let fit = if cover { Fit::Cover } else { Fit::Contain };
            let ds = DragStart { rect: start, crop: Crop::default(), fit, source: Some((sw, sh)) };
            let crop = drag(&ds, DragOp::Crop { edge }, dx, dy, false, None).crop;
            prop_assert!(sw - crop.left - crop.right >= MIN_CROP_SOURCE);
            prop_assert!(sh - crop.top - crop.bottom >= MIN_CROP_SOURCE);
            // And again from that crop: still 16 px.
            let again = drag(&DragStart { crop, ..ds }, DragOp::Crop { edge }, dx, dy, false, None).crop;
            prop_assert!(sw - again.left - again.right >= MIN_CROP_SOURCE);
            prop_assert!(sh - again.top - again.bottom >= MIN_CROP_SOURCE);
        }
    }
}
