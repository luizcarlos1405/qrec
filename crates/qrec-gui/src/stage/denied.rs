//! S24, a screen the portal refused (BRIEF S24, G§4.6): over each refused
//! screen layer's area of the live picture, the `muted-20` hatch with a
//! centered block: the 32 px `qrec-monitor-x-symbolic`, "Screen access
//! denied" (`title`; "Screen picker closed" when the person closed the
//! picker), the source "DP-4 · GIGA-BYTE GS27QA" (`caption` at 80 %; for a
//! closed picker "Choose DP-4 in your desktop's dialog to record it.") and
//! the secondary Try again with `record.toggle`'s key. Try again runs that
//! binding as `R` does, through the key router and the Session
//! (`Session::dispatch`): a take asks the portal for its screens again
//! (S25 #11's Try again starts the take again too). Owner: WP4.
//!
//! The engine leaves a refused screen out of the live picture, which shows
//! the canvas background in its place and the other sources over it (S24:
//! L2 Face live on top). [`denied`] is the GTK-free part: the blocks, and
//! the area to hatch, which the picture draws under the canvas overlay:
//! the refused layers' areas minus the pictures of the live layers above
//! them, layer by layer from the bottom, as disjoint rects (a layer on top
//! stays unhatched; overlapping refused layers hatch once).
//! [`DeniedBlocks`] shows the blocks on the preview, right above the canvas
//! overlay (their Try again takes clicks), each centered on its layer's
//! area within the preview; only while no take is counting down, starting
//! or running (the countdown's ring takes the preview's middle, and `R`
//! cancels then).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;
use qrec_core::LayerId;
use qrec_core::model::{Canvas, Scene, VideoSource};
use qrec_core::view::canvas::{layer_boxes, source_name};
use qrec_core::view::panel::PICKER_CLOSED;
use qrec_core::view::{LiveSourceState, LiveSources, Rect};

use crate::widgets::button::{self, Kind};
use crate::{Ctx, guarded};

/// The block's title for a denied screen (S24); a closed picker's is the
/// panel row's [`PICKER_CLOSED`] (BRIEF S24's variant).
pub const ACCESS_DENIED: &str = "Screen access denied";
/// What Try again runs: `R`'s binding (S24 draws its key on the button).
pub const RETRY_BINDING: &str = "record.toggle";

/// One refused screen layer's block.
#[derive(Debug, Clone, PartialEq)]
pub struct DeniedBlock {
    pub layer: LayerId,
    pub title: &'static str,
    pub caption: String,
    /// The layer's area on the canvas (canvas px), within the canvas.
    pub area: Rect,
}

/// What a refused screen shows in the live picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Denied {
    /// One per refused visible screen layer, bottom first.
    pub blocks: Vec<DeniedBlock>,
    /// The area to hatch (canvas px): disjoint rects within the canvas.
    pub hatch: Vec<Rect>,
}

/// A refused screen's source line: "DP-4 · GIGA-BYTE GS27QA" (the output
/// alone when the label is empty or the output itself, as the panel's
/// source line).
pub fn source_caption(output: &str, label: &str) -> String {
    if label.is_empty() || label == output {
        output.to_owned()
    } else {
        format!("{output} \u{b7} {label}")
    }
}

/// A closed picker's line (BRIEF S24's variant note).
pub fn picker_closed_caption(output: &str) -> String {
    format!("Choose {output} in your desktop's dialog to record it.")
}

/// The blocks and the hatch of `scene`'s live picture on `canvas` with the
/// live statuses `live`: each visible screen layer the portal refused gets
/// a block and adds its area to the hatch; each visible layer whose source
/// is live takes its picture out of the hatch below it.
pub fn denied(scene: &Scene, canvas: &Canvas, live: &LiveSources) -> Denied {
    let bounds = Rect::new(0.0, 0.0, f64::from(canvas.width), f64::from(canvas.height));
    let sizes = scene
        .layers
        .iter()
        .filter_map(|layer| {
            let size = live.get(source_name(&layer.source))?.size?;
            Some((layer.id.clone(), size))
        })
        .collect();
    // A "preview" the canvas's size: the boxes in canvas px.
    let boxes = layer_boxes(scene, canvas, &sizes, bounds);
    let mut out = Denied::default();
    for (layer, b) in scene.layers.iter().zip(&boxes) {
        if !layer.visible {
            continue;
        }
        let state = live.get(source_name(&layer.source)).map(|v| &v.state);
        match (&layer.source, state) {
            (
                VideoSource::Screen { output, label },
                Some(LiveSourceState::Refused { cancelled, .. }),
            ) => {
                let Some(area) = intersect(b.frame, bounds) else {
                    continue;
                };
                out.hatch = union(out.hatch, area);
                out.blocks.push(DeniedBlock {
                    layer: layer.id.clone(),
                    title: if *cancelled {
                        PICKER_CLOSED
                    } else {
                        ACCESS_DENIED
                    },
                    caption: if *cancelled {
                        picker_closed_caption(output)
                    } else {
                        source_caption(output, label)
                    },
                    area,
                });
            }
            (_, Some(LiveSourceState::Live)) => out.hatch = subtract_all(out.hatch, b.rect),
            _ => {}
        }
    }
    out
}

/// Where a block of `size` goes on a preview of `preview` (both in the
/// same px): centered on `area`, kept inside the preview (centered on it
/// when it doesn't fit).
pub fn block_origin(area: Rect, size: (f64, f64), preview: (f64, f64)) -> (f64, f64) {
    let place = |center: f64, len: f64, room: f64| {
        if len >= room {
            (room - len) / 2.0
        } else {
            (center - len / 2.0).clamp(0.0, room - len)
        }
    };
    (
        place(area.x + area.w / 2.0, size.0, preview.0),
        place(area.y + area.h / 2.0, size.1, preview.1),
    )
}

/// `a ∩ b`, if it has an area.
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = ((a.x + a.w).min(b.x + b.w), (a.y + a.h).min(b.y + b.h));
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// `a` minus `b`: up to four disjoint rects (the bands above and below
/// `b`, then its left and right of `a` between them).
fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let Some(cut) = intersect(a, b) else {
        return vec![a];
    };
    let (ax1, ay1) = (a.x + a.w, a.y + a.h);
    let (cx1, cy1) = (cut.x + cut.w, cut.y + cut.h);
    [
        Rect::new(a.x, a.y, a.w, cut.y - a.y),
        Rect::new(a.x, cy1, a.w, ay1 - cy1),
        Rect::new(a.x, cut.y, cut.x - a.x, cut.h),
        Rect::new(cx1, cut.y, ax1 - cx1, cut.h),
    ]
    .into_iter()
    .filter(|r| r.w > 0.0 && r.h > 0.0)
    .collect()
}

/// Every rect of `region` minus `b`.
fn subtract_all(region: Vec<Rect>, b: Rect) -> Vec<Rect> {
    region.into_iter().flat_map(|r| subtract(r, b)).collect()
}

/// `region` with `area` added, still disjoint.
fn union(region: Vec<Rect>, area: Rect) -> Vec<Rect> {
    let mut out = subtract_all(region, area);
    out.push(area);
    out
}

// ------------------------------------------------------------------ GTK

/// A block on the preview.
struct Shown {
    block: DeniedBlock,
    root: gtk::Box,
    retry: gtk::Button,
}

/// The blocks on the preview (`denied-<layer>`, its Try again
/// `denied-try-again-<layer>`, for the nested cases).
pub struct DeniedBlocks {
    ctx: Ctx,
    preview: gtk::Overlay,
    /// The blocks go right after it in the preview's children: over the
    /// canvas overlay, under the readout, the tags and the plates.
    anchor: gtk::Widget,
    shown: RefCell<Vec<Shown>>,
}

impl DeniedBlocks {
    pub fn new(ctx: &Ctx, preview: &gtk::Overlay, anchor: &impl IsA<gtk::Widget>) -> Rc<Self> {
        Rc::new(Self {
            ctx: ctx.clone(),
            preview: preview.clone(),
            anchor: anchor.clone().upcast(),
            shown: RefCell::new(Vec::new()),
        })
    }

    /// Shows `blocks`; `retry` is whether Try again can run now (`Err`:
    /// why not, its tooltip). Whether the blocks' places changed (the
    /// preview allocates them again).
    pub fn show(&self, blocks: &[DeniedBlock], retry: Result<(), String>) -> bool {
        let same = {
            let shown = self.shown.borrow();
            shown.len() == blocks.len()
                && shown.iter().zip(blocks).all(|(s, b)| {
                    s.block.layer == b.layer
                        && s.block.title == b.title
                        && s.block.caption == b.caption
                })
        };
        let moved = if same {
            let mut shown = self.shown.borrow_mut();
            let mut moved = false;
            for (s, b) in shown.iter_mut().zip(blocks) {
                if s.block.area != b.area {
                    s.block.area = b.area;
                    moved = true;
                }
            }
            moved
        } else {
            for old in self.shown.borrow_mut().drain(..) {
                self.preview.remove_overlay(&old.root);
            }
            let mut after = self.anchor.clone();
            let mut shown = Vec::with_capacity(blocks.len());
            for block in blocks {
                let s = self.build(block);
                s.root.insert_after(&self.preview, Some(&after));
                after = s.root.clone().upcast();
                shown.push(s);
            }
            *self.shown.borrow_mut() = shown;
            true
        };
        let why_not = retry.as_ref().err().map(String::as_str);
        for s in self.shown.borrow().iter() {
            if s.retry.is_sensitive() != why_not.is_none() {
                s.retry.set_sensitive(why_not.is_none());
            }
            if s.retry.tooltip_text().as_deref() != why_not {
                s.retry.set_tooltip_text(why_not);
            }
        }
        moved
    }

    fn build(&self, block: &DeniedBlock) -> Shown {
        // A group named by what it says (its labels follow inside).
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .css_classes(["stage-denied"])
            .name(format!("denied-{}", block.layer))
            .build();
        let icon = gtk::Image::builder()
            .icon_name("qrec-monitor-x-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .halign(gtk::Align::Center)
            .build();
        root.append(&icon);
        let title = gtk::Label::new(Some(block.title));
        title.add_css_class("t-title");
        title.add_css_class("stage-denied-title");
        root.append(&title);
        let caption = gtk::Label::new(Some(&block.caption));
        caption.add_css_class("t-caption");
        caption.add_css_class("stage-denied-caption");
        root.append(&caption);
        let keymap = self.ctx.store.keymap();
        let retry = button::labeled(
            "Try again",
            None,
            Some(RETRY_BINDING),
            &keymap,
            Kind::Secondary,
        );
        retry.set_widget_name(&format!("denied-try-again-{}", block.layer));
        retry.set_halign(gtk::Align::Center);
        let keys = self.ctx.keys.clone();
        retry.connect_clicked(guarded!("stage: a refused screen's Try again", move |_| {
            tracing::info!("stage: Try again on a refused screen runs {RETRY_BINDING}");
            keys.run_binding(RETRY_BINDING);
        }));
        root.append(&retry);
        root.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}: {}",
            block.title, block.caption
        ))]);
        Shown {
            block: block.clone(),
            root,
            retry,
        }
    }

    /// The place of `child` if it is a block: centered on its layer's area
    /// (canvas px on a canvas of `canvas`) on a preview of `preview` px.
    pub fn place(
        &self,
        child: &gtk::Widget,
        preview: (f64, f64),
        canvas: (f64, f64),
    ) -> Option<gdk::Rectangle> {
        let shown = self.shown.borrow();
        let s = shown
            .iter()
            .find(|s| s.root.upcast_ref::<gtk::Widget>() == child)?;
        if canvas.0 <= 0.0 || canvas.1 <= 0.0 {
            return None;
        }
        let (sx, sy) = (preview.0 / canvas.0, preview.1 / canvas.1);
        let a = s.block.area;
        let area = Rect::new(a.x * sx, a.y * sy, a.w * sx, a.h * sy);
        let (_, natural) = child.preferred_size();
        let size = (f64::from(natural.width()), f64::from(natural.height()));
        let (x, y) = block_origin(area, size, preview);
        Some(gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            natural.width(),
            natural.height(),
        ))
    }

    /// Whether `child` of the preview is one of the blocks (the stage
    /// places them with [`DeniedBlocks::place`]).
    pub fn is_block(&self, child: &gtk::Widget) -> bool {
        self.shown
            .borrow()
            .iter()
            .any(|s| s.root.upcast_ref::<gtk::Widget>() == child)
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::testkit;
    use qrec_core::view::LiveSourceView;

    use super::*;

    fn view(state: LiveSourceState, size: Option<(u32, u32)>) -> LiveSourceView {
        LiveSourceView { state, size }
    }

    fn refused(cancelled: bool) -> LiveSourceView {
        view(
            LiveSourceState::Refused {
                cancelled,
                message: "screen sharing for DP-4 failed".into(),
            },
            None,
        )
    }

    /// The design scene's statuses: DP-4 refused (or `cancelled`), the
    /// camera live at 1280×720.
    fn s24(cancelled: bool) -> LiveSources {
        let mut live = LiveSources::new();
        live.insert("DP-4".into(), refused(cancelled));
        live.insert(
            "/dev/video0".into(),
            view(LiveSourceState::Live, Some((1280, 720))),
        );
        live
    }

    fn area(rects: &[Rect]) -> f64 {
        rects.iter().map(|r| r.w * r.h).sum()
    }

    fn inside(rects: &[Rect], x: f64, y: f64) -> usize {
        rects.iter().filter(|r| r.contains(x, y)).count()
    }

    /// S24: L1 (DP-4, the whole canvas) refused under L2 Face (live): one
    /// block over the whole canvas, "Screen access denied" with L1's
    /// source; the hatch is the canvas minus L2's box (1500, 760, 384×288).
    #[test]
    fn s24_hatches_l1_around_l2() {
        let project = testkit::design_project();
        let d = denied(&project.scene, &project.canvas, &s24(false));
        assert_eq!(
            d.blocks,
            [DeniedBlock {
                layer: LayerId::numbered(1),
                title: "Screen access denied",
                caption: "DP-4 \u{b7} GIGA-BYTE GS27QA".into(),
                area: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            }]
        );
        assert_eq!(area(&d.hatch), 1920.0 * 1080.0 - 384.0 * 288.0);
        for (x, y, hatched) in [
            (10.0, 10.0, true),
            (1499.0, 900.0, true),
            (1500.0, 760.0, false),
            (1700.0, 900.0, false),
            (1883.0, 1047.0, false),
            (1884.0, 1047.0, true),
            (1700.0, 1048.0, true),
            (1919.0, 1079.0, true),
        ] {
            assert_eq!(inside(&d.hatch, x, y), usize::from(hatched), "({x}, {y})");
        }
    }

    /// BRIEF S24's variant: the picker closed.
    #[test]
    fn a_closed_picker_says_what_to_choose() {
        let project = testkit::design_project();
        let d = denied(&project.scene, &project.canvas, &s24(true));
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].title, "Screen picker closed");
        assert_eq!(
            d.blocks[0].caption,
            "Choose DP-4 in your desktop's dialog to record it."
        );
    }

    /// Only a refused screen of a visible layer: a hidden one, a screen
    /// waiting for the portal or live, a dead camera show nothing; a layer
    /// above that isn't live (dead, asked again) leaves the hatch whole.
    #[test]
    fn only_visible_refused_screens_show() {
        let project = testkit::design_project();
        let (scene, canvas) = (&project.scene, &project.canvas);
        let mut hidden = scene.clone();
        hidden.layers[0].visible = false;
        assert_eq!(denied(&hidden, canvas, &s24(false)), Denied::default());
        for state in [LiveSourceState::AwaitingPortal, LiveSourceState::Live] {
            let mut live = s24(false);
            live.insert("DP-4".into(), view(state, Some((2560, 1440))));
            assert_eq!(denied(scene, canvas, &live), Denied::default());
        }
        assert_eq!(
            denied(scene, canvas, &LiveSources::new()),
            Denied::default(),
            "not reported yet"
        );
        let mut dead_camera = s24(false);
        dead_camera.insert(
            "/dev/video0".into(),
            view(
                LiveSourceState::Dead {
                    message: "gone".into(),
                },
                None,
            ),
        );
        let d = denied(scene, canvas, &dead_camera);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.hatch, [Rect::new(0.0, 0.0, 1920.0, 1080.0)]);
        // A camera can't be refused (only screens go through the portal).
        let mut camera_on_top_refused = s24(false);
        camera_on_top_refused.insert("/dev/video0".into(), refused(false));
        let d = denied(scene, canvas, &camera_on_top_refused);
        assert_eq!(d.blocks.len(), 1, "the screen's block only");
        assert_eq!(area(&d.hatch), 1920.0 * 1080.0, "nothing live on top");
    }

    /// The order matters: a refused screen over a live layer hatches over
    /// it; overlapping refused screens hatch once (disjoint rects); a
    /// layer past the canvas hatches its part on it. Checked at points of
    /// a grid against the layers' boxes painted bottom to top.
    #[test]
    fn the_hatch_follows_the_layers_bottom_to_top() {
        let base = testkit::design_project();
        let canvas = base.canvas.clone();
        let screen = |id: u32, output: &str, rect: qrec_core::model::Rect| {
            let mut layer = base.scene.layers[0].clone();
            layer.id = LayerId::numbered(id);
            layer.source = VideoSource::Screen {
                output: output.into(),
                label: String::new(),
            };
            layer.rect = rect;
            layer
        };
        let camera = |id: u32, device: &str, rect: qrec_core::model::Rect| {
            let mut layer = base.scene.layers[1].clone();
            layer.id = LayerId::numbered(id);
            layer.source = VideoSource::Camera {
                device: device.into(),
                label: String::new(),
            };
            layer.rect = rect;
            layer
        };
        use qrec_core::model::Rect as R;
        let mut scene = base.scene.clone();
        scene.layers = vec![
            camera(1, "/dev/a", R::new(0, 0, 960, 540)),
            screen(2, "DP-1", R::new(200, 100, 1000, 600)),
            camera(3, "/dev/b", R::new(800, 300, 300, 300)),
            screen(4, "DP-2", R::new(1000, 400, 1200, 800)),
            camera(5, "/dev/c", R::new(1700, 900, 400, 400)),
        ];
        let mut live = LiveSources::new();
        for device in ["/dev/a", "/dev/b", "/dev/c"] {
            live.insert(device.into(), view(LiveSourceState::Live, None));
        }
        live.insert("DP-1".into(), refused(false));
        live.insert("DP-2".into(), refused(true));
        let d = denied(&scene, &canvas, &live);
        assert_eq!(
            d.blocks
                .iter()
                .map(|b| b.layer.to_string())
                .collect::<Vec<_>>(),
            ["L2", "L4"]
        );
        assert_eq!(d.blocks[0].caption, "DP-1");
        assert_eq!(
            d.blocks[1].area,
            Rect::new(1000.0, 400.0, 920.0, 680.0),
            "within the canvas"
        );
        for x in (0..1920).step_by(20) {
            for y in (0..1080).step_by(20) {
                let (x, y) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                // The topmost layer at the point decides.
                let top = scene.layers.iter().rev().find(|l| {
                    let r = l.rect;
                    Rect::new(
                        f64::from(r.x),
                        f64::from(r.y),
                        f64::from(r.w),
                        f64::from(r.h),
                    )
                    .contains(x, y)
                });
                let want = top.is_some_and(|l| matches!(l.source, VideoSource::Screen { .. }));
                assert_eq!(
                    inside(&d.hatch, x, y),
                    usize::from(want),
                    "({x}, {y}) under {:?}",
                    top.map(|l| l.id.to_string())
                );
            }
        }
    }

    #[test]
    fn source_captions() {
        assert_eq!(
            source_caption("DP-4", "GIGA-BYTE GS27QA"),
            "DP-4 \u{b7} GIGA-BYTE GS27QA"
        );
        assert_eq!(source_caption("DP-4", ""), "DP-4");
        assert_eq!(source_caption("DP-4", "DP-4"), "DP-4");
    }

    /// A block is centered on its area, kept on the preview, centered on it
    /// when it can't fit.
    #[test]
    fn blocks_sit_centered_on_their_area() {
        let preview = (960.0, 540.0);
        // S24: the plate 223×190 on the whole preview: (368.5, 175).
        assert_eq!(
            block_origin(Rect::new(0.0, 0.0, 960.0, 540.0), (223.0, 190.0), preview),
            (368.5, 175.0)
        );
        // A small area in the corner: kept inside.
        assert_eq!(
            block_origin(Rect::new(900.0, 0.0, 60.0, 40.0), (223.0, 190.0), preview),
            (737.0, 0.0)
        );
        // Wider than the preview: centered on it.
        assert_eq!(
            block_origin(Rect::new(0.0, 0.0, 100.0, 100.0), (1000.0, 190.0), preview),
            (-20.0, 0.0)
        );
    }
}
