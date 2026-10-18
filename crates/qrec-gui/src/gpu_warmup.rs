//! The GPU renderer's warm-up (G§8.6). GTK's GL renderer compiles a shader
//! program the first time it draws a kind of thing (an op: a color, a
//! rounded color, a border, a shadow, glyphs, a texture as it is or
//! converted from its color state; under no clip, a rectangle's or a rounded
//! one), on the main thread. With the driver's shader cache cold (a first
//! launch, a Mesa or GTK update, and every nested test run, whose
//! `XDG_CACHE_HOME` is new) one takes 30–70 ms, and the first ones land
//! mid-session: the stage's plates when a take starts, the Program's first
//! frame, a clip's label under a clip, a toast's rounded corners (WP10's
//! gles2 smoke: stalls of 50–90 ms at steps 4–8, each right after a
//! `GSK_DEBUG=shaders` compile).
//!
//! Right after the window's first frame, [`start`] draws the [`cells`],
//! offscreen with the window's own renderer (its programs are the
//! window's) and one per idle callback: every kind the window draws later
//! under each clip mode, so they compile while the project opens (the
//! test channel's `waitIdle` waits for the last one, [`pending`]). Cold,
//! the 39 take about 2 s in all (one held the loop 60 ms at most); with the
//! cache warm, a few milliseconds. The Cairo renderer (pixman runs:
//! `GDK_DISABLE=gl`) compiles nothing, so nothing runs there.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use crate::guard;

/// A cell of the scene: what one kind draws in, under one clip.
const CELL: f32 = 16.0;
/// The space between two cells, and around them all (an outset shadow
/// spreads 3 px past its cell).
const GAP: f32 = 4.0;

/// How a kind is clipped: GTK compiles a program per clip mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    /// No clip (or one the drawing fits in).
    None,
    /// A rectangle the drawing crosses, off the pixel grid (a scissor can't
    /// do it, at any scale: 0.3 px in).
    Rect,
    /// A rounded rectangle the drawing crosses.
    Rounded,
}

impl Clip {
    pub const ALL: [Clip; 3] = [Clip::None, Clip::Rect, Clip::Rounded];
}

/// What the window draws, each a program of its own (the CSS backgrounds,
/// borders, shadows and outlines, the text, the icons and pictures, the
/// thumbnails, the live picture and the Program's frames, the timeline's
/// paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Color,
    RoundedColor,
    Border,
    OutsetShadow,
    InsetShadow,
    LinearGradient,
    Text,
    Fill,
    Stroke,
    /// A premultiplied sRGB texture (icons, art).
    Texture,
    /// A straight-alpha RGBA texture (thumbnails, the live picture's
    /// frames), converted as it is drawn.
    StraightTexture,
    /// A straight-alpha texture in a CICP color state (the Program's
    /// frames, BT.709).
    CicpTexture,
    /// The straight and CICP ones at an opacity (a disabled row's).
    FadedTextures,
}

impl Kind {
    pub const ALL: [Kind; 13] = [
        Kind::Color,
        Kind::RoundedColor,
        Kind::Border,
        Kind::OutsetShadow,
        Kind::InsetShadow,
        Kind::LinearGradient,
        Kind::Text,
        Kind::Fill,
        Kind::Stroke,
        Kind::Texture,
        Kind::StraightTexture,
        Kind::CicpTexture,
        Kind::FadedTextures,
    ];
}

/// The scene's size: a row per clip mode, a column per kind.
pub fn size() -> (f32, f32) {
    let span = |n: usize| GAP + n as f32 * (CELL + GAP);
    (span(Kind::ALL.len()), span(Clip::ALL.len()))
}

/// The textures the scene draws, 2×2 px each.
struct Textures {
    premultiplied: gdk::Texture,
    straight: gdk::Texture,
    cicp: Option<gdk::Texture>,
}

impl Textures {
    fn new() -> Self {
        // Half-transparent, so a straight texture needs its conversion.
        let pixels = glib::Bytes::from_owned(
            [200u8, 120, 40, 160]
                .iter()
                .copied()
                .cycle()
                .take(16)
                .collect::<Vec<_>>(),
        );
        let texture = |format: gdk::MemoryFormat| -> gdk::Texture {
            gdk::MemoryTexture::new(2, 2, format, &pixels, 8).upcast()
        };
        // BT.709 RGB at full range: what the Program's sink hands GTK for a
        // decoded take.
        let cicp = gdk::CicpParams::new();
        cicp.set_color_primaries(1);
        cicp.set_transfer_function(1);
        cicp.set_matrix_coefficients(0);
        cicp.set_range(gdk::CicpRange::Full);
        let cicp = cicp.build_color_state().ok().map(|state| {
            gdk::MemoryTextureBuilder::new()
                .set_width(2)
                .set_height(2)
                .set_format(gdk::MemoryFormat::R8g8b8a8)
                .set_bytes(Some(&pixels))
                .set_stride(8)
                .set_color_state(&state)
                .build()
        });
        Self {
            premultiplied: texture(gdk::MemoryFormat::B8g8r8a8Premultiplied),
            straight: texture(gdk::MemoryFormat::R8g8b8a8),
            cicp,
        }
    }
}

/// Every [`Kind`] under every [`Clip`] in one node (the cells of
/// [`cells`]).
pub fn scene(context: &pango::Context) -> Option<gsk::RenderNode> {
    let cells = cells(context);
    (!cells.is_empty()).then(|| gsk::ContainerNode::new(&cells).upcast())
}

/// Every [`Kind`] under every [`Clip`], a node each in its cell of the
/// scene (a row per clip, a column per kind); the text laid out in
/// `context` (the window's: its fonts and font options, so its glyphs are
/// the window's). Made of GSK's nodes directly: nothing here needs a
/// display.
pub fn cells(context: &pango::Context) -> Vec<gsk::RenderNode> {
    let textures = Textures::new();
    // No font map, no text (Pango would complain about each run).
    let layout = context.font_map().map(|_| {
        let layout = pango::Layout::new(context);
        layout.set_text("Ag");
        layout
    });
    let mut cells = Vec::new();
    for (row, clip) in Clip::ALL.into_iter().enumerate() {
        for (column, kind) in Kind::ALL.into_iter().enumerate() {
            let x = GAP + column as f32 * (CELL + GAP);
            let y = GAP + row as f32 * (CELL + GAP);
            let cell = graphene::Rect::new(x, y, CELL, CELL);
            let Some(node) = draw(kind, &cell, &textures, layout.as_ref()) else {
                continue;
            };
            // 0.3 px in: off the pixel grid at any scale, so no scissor.
            let inside = graphene::Rect::new(x + 0.3, y + 0.3, CELL - 0.6, CELL - 0.6);
            cells.push(match clip {
                Clip::None => node,
                Clip::Rect => gsk::ClipNode::new(node, &inside).upcast(),
                Clip::Rounded => {
                    gsk::RoundedClipNode::new(node, &gsk::RoundedRect::from_rect(inside, 5.0))
                        .upcast()
                }
            });
        }
    }
    cells
}

/// One kind's node, filling (and so crossing the clips of) `cell`; `None`
/// for text without glyphs (no font map) or a CICP texture GDK can't make.
fn draw(
    kind: Kind,
    cell: &graphene::Rect,
    textures: &Textures,
    layout: Option<&pango::Layout>,
) -> Option<gsk::RenderNode> {
    let color = gdk::RGBA::new(0.8, 0.5, 0.2, 0.9);
    let rounded = gsk::RoundedRect::from_rect(*cell, 4.0);
    let node: gsk::RenderNode = match kind {
        Kind::Color => gsk::ColorNode::new(&color, cell).upcast(),
        Kind::RoundedColor => {
            gsk::RoundedClipNode::new(gsk::ColorNode::new(&color, cell), &rounded).upcast()
        }
        Kind::Border => gsk::BorderNode::new(&rounded, &[1.0; 4], &[color; 4]).upcast(),
        Kind::OutsetShadow => {
            let inner = gsk::RoundedRect::from_rect(cell.inset_r(3.0, 3.0), 3.0);
            gsk::OutsetShadowNode::new(&inner, &color, 1.0, 1.0, 1.0, 3.0).upcast()
        }
        Kind::InsetShadow => {
            gsk::InsetShadowNode::new(&rounded, &color, 1.0, 1.0, 1.0, 3.0).upcast()
        }
        Kind::LinearGradient => gsk::LinearGradientNode::new(
            cell,
            &cell.top_left(),
            &cell.bottom_right(),
            &[
                gsk::ColorStop::new(0.0, color),
                gsk::ColorStop::new(1.0, gdk::RGBA::new(0.1, 0.2, 0.3, 0.5)),
            ],
        )
        .upcast(),
        Kind::Text => text(layout?, cell.top_left(), &color)?,
        Kind::Fill | Kind::Stroke => {
            let path = gsk::PathBuilder::new();
            path.add_circle(&cell.center(), CELL / 2.0);
            let path = path.to_path();
            let paint = gsk::ColorNode::new(&color, cell);
            if kind == Kind::Fill {
                gsk::FillNode::new(paint, &path, gsk::FillRule::Winding).upcast()
            } else {
                gsk::StrokeNode::new(paint, &path, &gsk::Stroke::new(1.5)).upcast()
            }
        }
        Kind::Texture => gsk::TextureNode::new(&textures.premultiplied, cell).upcast(),
        Kind::StraightTexture => gsk::TextureNode::new(&textures.straight, cell).upcast(),
        Kind::CicpTexture => gsk::TextureNode::new(textures.cicp.as_ref()?, cell).upcast(),
        Kind::FadedTextures => {
            let mut faded: Vec<gsk::RenderNode> =
                vec![gsk::TextureNode::new(&textures.straight, cell).upcast()];
            if let Some(cicp) = &textures.cicp {
                faded.push(gsk::TextureNode::new(cicp, cell).upcast());
            }
            gsk::OpacityNode::new(gsk::ContainerNode::new(&faded), 0.5).upcast()
        }
    };
    Some(node)
}

/// `layout`'s glyph runs as text nodes, its top left at `origin` (what
/// `gtk::Snapshot::append_layout` makes); `None` without glyphs.
fn text(
    layout: &pango::Layout,
    origin: graphene::Point,
    color: &gdk::RGBA,
) -> Option<gsk::RenderNode> {
    let px = |units: i32| units as f32 / pango::SCALE as f32;
    let mut runs: Vec<gsk::RenderNode> = Vec::new();
    let mut iter = layout.iter();
    loop {
        if let Some(run) = iter.run_readonly() {
            let font = run.item().analysis().font();
            let glyphs = run.glyph_string();
            let (_, logical) = iter.run_extents();
            let at = graphene::Point::new(
                origin.x() + px(logical.x()),
                origin.y() + px(iter.baseline()),
            );
            if let Some(node) = gsk::TextNode::new(&font, &glyphs, color, &at) {
                runs.push(node.upcast());
            }
        }
        if !iter.next_run() {
            break;
        }
    }
    (!runs.is_empty()).then(|| gsk::ContainerNode::new(&runs).upcast())
}

thread_local! {
    /// The warm-up is due: started, its frame not drawn yet.
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the warm-up is due (the test channel's `waitIdle` waits for
/// it).
pub fn pending() -> bool {
    PENDING.with(Cell::get)
}

/// After the window's next frame (its first, from `activate`), draws the
/// [`cells`] with the window's renderer, unless that is the Cairo renderer:
/// one cell per idle callback (a cold compile is 30–70 ms; all of them in
/// one dispatch held the loop for 2 s), between the window's frames and
/// whatever else the opening does.
pub fn start(window: &gtk::Window) {
    let Some(clock) = window.frame_clock() else {
        tracing::debug!("gpu warm-up: the window has no frame clock yet; skipped");
        return;
    };
    PENDING.with(|p| p.set(true));
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let slot = Rc::clone(&handler);
    let weak = window.downgrade();
    let id = clock.connect_after_paint(crate::guarded!(
        "gpu warm-up: after the first frame",
        move |clock| {
            if let Some(id) = slot.borrow_mut().take() {
                clock.disconnect(id);
            }
            let Some(window) = weak.upgrade() else {
                PENDING.with(|p| p.set(false));
                return;
            };
            let cells = match window.renderer() {
                Some(renderer) if !renderer.is::<gsk::CairoRenderer>() => {
                    let mut cells = cells(&window.pango_context());
                    cells.reverse();
                    Some((renderer, cells))
                }
                _ => None,
            };
            match cells {
                Some((renderer, cells)) => {
                    let total = cells.len();
                    step(renderer, cells, total, Instant::now());
                }
                None => {
                    tracing::debug!("gpu warm-up: the Cairo renderer compiles nothing; skipped");
                    PENDING.with(|p| p.set(false));
                }
            }
        }
    ));
    *handler.borrow_mut() = Some(id);
    window.queue_draw();
}

/// Draws the next of the cells `left` (the last one) in an idle callback
/// of its own, then the one after, and so on: offscreen, with the window's
/// `renderer` (GL or Vulkan; its programs are the window's).
fn step(renderer: gsk::Renderer, mut left: Vec<gsk::RenderNode>, total: usize, started: Instant) {
    guard::idle_local_once("gpu warm-up", move || {
        let Some(cell) = left.pop() else {
            PENDING.with(|p| p.set(false));
            tracing::info!(
                "gpu warm-up: {total} kinds under their clips drawn with {} in {} ms",
                renderer.type_().name(),
                started.elapsed().as_millis()
            );
            return;
        };
        let _ = renderer.render_texture(&cell, Some(&cell.bounds()));
        step(renderer, left, total, started);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(node: &gsk::RenderNode, found: &mut Vec<gsk::RenderNodeType>) {
        found.push(node.node_type());
        if let Some(container) = node.downcast_ref::<gsk::ContainerNode>() {
            for i in 0..container.n_children() {
                kinds(&container.child(i), found);
            }
        } else if let Some(clip) = node.downcast_ref::<gsk::ClipNode>() {
            kinds(&clip.child(), found);
        } else if let Some(clip) = node.downcast_ref::<gsk::RoundedClipNode>() {
            kinds(&clip.child(), found);
        } else if let Some(opacity) = node.downcast_ref::<gsk::OpacityNode>() {
            kinds(&opacity.child(), found);
        } else if let Some(fill) = node.downcast_ref::<gsk::FillNode>() {
            kinds(&fill.child(), found);
        } else if let Some(stroke) = node.downcast_ref::<gsk::StrokeNode>() {
            kinds(&stroke.child(), found);
        }
    }

    /// A row per clip mode, a column per kind (but the text, which needs
    /// the window's fonts): every kind's node is there under each clip, in
    /// the scene's bounds, and the three CICP-or-not textures each draw.
    #[test]
    fn the_scene_has_every_kind_under_every_clip() {
        let node = scene(&pango::Context::new()).expect("a scene");
        let (width, height) = size();
        let bounds = node.bounds();
        assert!(
            bounds.x() >= 0.0
                && bounds.y() >= 0.0
                && bounds.x() + bounds.width() <= width
                && bounds.y() + bounds.height() <= height,
            "{bounds:?} in {width}×{height}"
        );
        let mut found = Vec::new();
        kinds(&node, &mut found);
        let count = |kind: gsk::RenderNodeType| found.iter().filter(|k| **k == kind).count();
        let per_clip = Clip::ALL.len();
        // Color (and the rounded color's, the fill's and the stroke's paint).
        assert_eq!(count(gsk::RenderNodeType::ColorNode), 4 * per_clip);
        for kind in [
            gsk::RenderNodeType::BorderNode,
            gsk::RenderNodeType::OutsetShadowNode,
            gsk::RenderNodeType::InsetShadowNode,
            gsk::RenderNodeType::LinearGradientNode,
            gsk::RenderNodeType::FillNode,
            gsk::RenderNodeType::StrokeNode,
            gsk::RenderNodeType::OpacityNode,
        ] {
            assert_eq!(count(kind), per_clip, "{kind:?} in {found:?}");
        }
        // Premultiplied, straight, CICP, and the faded straight and CICP.
        assert_eq!(count(gsk::RenderNodeType::TextureNode), 5 * per_clip);
        // The rect clips; the rounded ones, and the rounded color's own.
        let cells = Kind::ALL.len() - 1;
        assert_eq!(count(gsk::RenderNodeType::ClipNode), cells);
        assert_eq!(
            count(gsk::RenderNodeType::RoundedClipNode),
            cells + per_clip
        );
    }
}
