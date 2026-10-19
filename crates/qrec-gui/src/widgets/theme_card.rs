//! C32 theme card (G§5.1, S21): a theme's mini window, drawn from that
//! theme's `Palette`, with its name under it. Owner: WP9.
//!
//! S21's card (pen `qqnCi`), 180×132 × the UI scale: the theme's `surface`
//! with a 1 px `border` (selected: 2 px `accent` and the `circle-check`),
//! `lg` radius, the stroke inside, over the padding (what is in the card
//! sits from its outer edge, as S21 draws every card); 8 px in, the 164×92
//! preview (`md` radius, the theme's
//! `bg`): the header, the Record pill, the wordmark, the panel with a
//! selected row, a muted line and a meter, the canvas (the frames' screen,
//! `resources/art/theme-card-canvas.png`), the toolbar, four take-colored
//! clips with the selection and the playhead, a lily lane with its wave,
//! the hint bar; then the 18 px name row 6 under it: the name (13/18 600,
//! the theme's `text`), "Default" (12/16, its `muted`) 6 after it, the
//! check at the right. A card is a `gtk::ToggleButton` (grouped, one on):
//! the keyboard reaches it, Space and Enter pick it, screen readers hear
//! "Swamp Night" and whether it is on; it draws the focus ring.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk, pango};
use qrec_core::config::Theme as ThemeId;

use crate::theme::Palette;
use crate::widgets::draw;

/// The card's size at 100 % (S21).
pub const SIZE: (f64, f64) = (180.0, 132.0);

/// The mini canvas image in the GResource (144×80: 2× its 72×40).
pub const CANVAS: &str = "/com/rsensato/qrec/art/theme-card-canvas.png";

/// A rect of the preview (164×92, its top-left 0, 0) and its color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    pub rect: [f64; 4],
    pub radius: f64,
    pub color: gdk::RGBA,
}

/// The preview's flat parts in paint order, in `palette`'s colors (S21's
/// `Preview`, GTK-free): what the card draws but the canvas image, the
/// selection's stroke and the playhead.
pub fn preview_parts(palette: &Palette) -> Vec<Part> {
    let part = |rect: [f64; 4], radius: f64, color: gdk::RGBA| Part {
        rect,
        radius,
        color,
    };
    let mut parts = vec![
        part([0.0, 0.0, 164.0, 8.0], 0.0, palette.surface),
        part([128.0, 2.0, 18.0, 4.0], 2.0, palette.record),
        part([4.0, 3.0, 14.0, 2.0], 1.0, palette.text),
        part([124.0, 8.0, 40.0, 78.0], 0.0, palette.surface),
        part([128.0, 14.0, 32.0, 4.0], 1.0, palette.selected),
        part([128.0, 21.0, 24.0, 3.0], 1.0, palette.muted),
        part([128.0, 30.0, 28.0, 2.0], 0.0, palette.meter_green),
        part([0.0, 56.0, 124.0, 4.0], 0.0, palette.surface),
    ];
    // Four clips: moss, onion, lily and fairy (the take colors' 1st, 3rd,
    // 5th and 8th), each a `raised` body with its 2 px stripe.
    for (x, w, take) in [
        (0.0, 11.0, palette.take_moss),
        (12.0, 21.0, palette.take_onion),
        (34.0, 29.0, palette.take_lily),
        (64.0, 59.0, palette.take_fairy),
    ] {
        parts.push(part([x, 62.0, w, 9.0], 0.0, palette.raised));
        parts.push(part([x, 62.0, w, 2.0], 0.0, take));
    }
    parts.push(part([0.0, 74.0, 124.0, 9.0], 0.0, palette.take_lily_tint));
    parts.push(part([4.0, 77.0, 116.0, 3.0], 1.0, palette.wave));
    parts.push(part([0.0, 86.0, 164.0, 6.0], 0.0, palette.surface));
    parts
}

/// The card's own colors (S21): its ground, its stroke and width, the
/// name's, the caption's, the check's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardColors {
    pub ground: gdk::RGBA,
    pub stroke: gdk::RGBA,
    pub stroke_width: f64,
    pub name: gdk::RGBA,
    pub caption: gdk::RGBA,
    pub check: gdk::RGBA,
}

/// The colors of a card of `palette`, selected or not (hovered: its stroke
/// `border-strong`, DESIGN's hover for a selectable card).
pub fn card_colors(palette: &Palette, selected: bool, hovered: bool) -> CardColors {
    CardColors {
        ground: palette.surface,
        stroke: if selected {
            palette.accent
        } else if hovered {
            palette.border_strong
        } else {
            palette.border
        },
        stroke_width: if selected { 2.0 } else { 1.0 },
        name: palette.text,
        caption: palette.muted,
        check: palette.accent,
    }
}

mod imp {
    use super::*;

    pub struct CardArt {
        pub theme: Cell<ThemeId>,
        pub name: RefCell<String>,
        pub caption: RefCell<Option<String>>,
        pub selected: Cell<bool>,
        pub hovered: Cell<bool>,
        pub scale: Cell<f64>,
        pub canvas: RefCell<Option<gdk::Texture>>,
    }

    impl Default for CardArt {
        fn default() -> Self {
            Self {
                theme: Cell::new(ThemeId::SwampNight),
                name: RefCell::new(String::new()),
                caption: RefCell::new(None),
                selected: Cell::new(false),
                hovered: Cell::new(false),
                scale: Cell::new(1.0),
                canvas: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardArt {
        const NAME: &'static str = "QrecThemeCardArt";
        type Type = super::CardArt;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("themecardart");
        }
    }

    impl ObjectImpl for CardArt {
        fn constructed(&self) {
            self.parent_constructed();
            crate::guard::run("theme card: constructed", || {
                let obj = self.obj();
                obj.set_can_focus(false);
                obj.set_focusable(false);
                obj.set_can_target(false);
                obj.set_accessible_role(gtk::AccessibleRole::Presentation);
                *self.canvas.borrow_mut() = gdk::Texture::from_resource(CANVAS).into();
            });
        }
    }

    impl WidgetImpl for CardArt {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let scale = self.scale.get();
            let size = match orientation {
                gtk::Orientation::Horizontal => SIZE.0,
                _ => SIZE.1,
            };
            let px = (size * scale).round() as i32;
            (px, px, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("theme card: snapshot", || self.draw(snapshot));
        }
    }

    impl CardArt {
        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let scale = self.scale.get() as f32;
            let palette = Palette::for_theme(self.theme.get());
            let colors = card_colors(&palette, self.selected.get(), self.hovered.get());
            snapshot.save();
            snapshot.scale(scale, scale);
            let (w, h) = (SIZE.0 as f32, SIZE.1 as f32);
            let b = colors.stroke_width as f32;
            // The stroke, then the ground inside it.
            draw::rounded_rect(
                snapshot,
                &graphene::Rect::new(0.0, 0.0, w, h),
                12.0,
                &colors.stroke,
            );
            draw::rounded_rect(
                snapshot,
                &graphene::Rect::new(b, b, w - 2.0 * b, h - 2.0 * b),
                12.0 - b,
                &colors.ground,
            );
            // The preview, 8 px in from the outer edge: the stroke is
            // inside, over the padding (the pen's; S21's preview at 8 px in
            // on every card, the selected one's 2 px stroke too).
            let (px, py) = (8.0, 8.0);
            let preview = graphene::Rect::new(px, py, 164.0, 92.0);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(preview, 6.0));
            snapshot.append_color(&palette.bg, &preview);
            let at = |r: [f64; 4]| {
                graphene::Rect::new(px + r[0] as f32, py + r[1] as f32, r[2] as f32, r[3] as f32)
            };
            for part in preview_parts(&palette) {
                if part.radius > 0.0 {
                    draw::rounded_rect(snapshot, &at(part.rect), part.radius as f32, &part.color);
                } else {
                    snapshot.append_color(&part.color, &at(part.rect));
                }
            }
            if let Some(canvas) = self.canvas.borrow().as_ref() {
                snapshot.append_scaled_texture(
                    canvas,
                    gsk::ScalingFilter::Trilinear,
                    &at([26.0, 12.0, 72.0, 40.0]),
                );
            }
            // The selection (1 px `accent`) and the playhead.
            let selection = at([34.0, 62.0, 30.0, 9.0]);
            let stroke = gsk::RoundedRect::from_rect(selection, 0.0);
            snapshot.append_border(&stroke, &[1.0; 4], &[palette.accent; 4]);
            snapshot.append_color(&palette.playhead, &at([48.0, 58.0, 1.5, 28.0]));
            snapshot.pop();

            // The name row: 6 under the preview, 18 tall.
            let row_y = py + 92.0 + 6.0;
            let name = self.name.borrow();
            let layout = text_layout(obj.upcast_ref(), &name, 13.0, pango::Weight::Semibold);
            let (_, logical) = layout.pixel_extents();
            let name_y = row_y + (18.0 - logical.height() as f32) / 2.0;
            draw::text(snapshot, &layout, px, name_y, &colors.name);
            if let Some(caption) = self.caption.borrow().as_deref() {
                let small = text_layout(obj.upcast_ref(), caption, 12.0, pango::Weight::Normal);
                let (_, cap) = small.pixel_extents();
                let x = px + logical.width() as f32 + 6.0;
                let y = row_y + (18.0 - cap.height() as f32) / 2.0;
                draw::text(snapshot, &small, x, y, &colors.caption);
            }
            if self.selected.get() {
                let icon_x = f64::from(w - 8.0 - 16.0);
                let icon_y = f64::from(row_y + 1.0);
                symbolic_icon(
                    snapshot,
                    obj.upcast_ref(),
                    "qrec-circle-check-symbolic",
                    (icon_x, icon_y),
                    16,
                    &colors.check,
                );
            }
            snapshot.restore();
        }
    }

    /// The symbolic icon `name` at `size` px, its top left at `(x, y)`, in
    /// `color` (as the timeline draws its icons).
    fn symbolic_icon(
        snapshot: &gtk::Snapshot,
        widget: &gtk::Widget,
        name: &str,
        (x, y): (f64, f64),
        size: i32,
        color: &gdk::RGBA,
    ) {
        let theme = gtk::IconTheme::for_display(&widget.display());
        let paintable = theme.lookup_icon(
            name,
            &[],
            size,
            widget.scale_factor(),
            gtk::TextDirection::Ltr,
            gtk::IconLookupFlags::empty(),
        );
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x as f32, y as f32));
        paintable.snapshot_symbolic(snapshot, f64::from(size), f64::from(size), &[*color]);
        snapshot.restore();
    }

    /// `text` in the UI face at `px` (100 % px; the card scales it).
    fn text_layout(
        widget: &gtk::Widget,
        text: &str,
        px: f64,
        weight: pango::Weight,
    ) -> pango::Layout {
        let layout = widget.create_pango_layout(Some(text));
        let mut font = pango::FontDescription::new();
        font.set_family("Atkinson Hyperlegible Next, Atkinson Hyperlegible, Cantarell, sans-serif");
        font.set_absolute_size(px * f64::from(pango::SCALE));
        font.set_weight(weight);
        layout.set_font_description(Some(&font));
        layout
    }
}

glib::wrapper! {
    /// What a theme card draws (its button is around it).
    pub struct CardArt(ObjectSubclass<imp::CardArt>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// A C32 theme card: the toggle button and what it draws.
pub struct ThemeCard {
    button: gtk::ToggleButton,
    art: CardArt,
}

impl ThemeCard {
    /// The card of `theme`, named `name` ("Swamp Night") with `caption`
    /// ("Default") after the name.
    pub fn new(theme: ThemeId, id: &str, name: &str, caption: Option<&str>) -> Self {
        let art: CardArt = glib::Object::new();
        let imp = art.imp();
        imp.theme.set(theme);
        *imp.name.borrow_mut() = name.to_owned();
        *imp.caption.borrow_mut() = caption.map(str::to_owned);
        let button = gtk::ToggleButton::new();
        button.set_widget_name(&format!("theme-card-{id}"));
        button.add_css_class("theme-card");
        button.set_child(Some(&art));
        button.set_focus_on_click(false);
        let spoken = match caption {
            Some(caption) => format!("{name} ({caption})"),
            None => name.to_owned(),
        };
        button.update_property(&[gtk::accessible::Property::Label(&spoken)]);
        let hover = gtk::EventControllerMotion::new();
        let weak = art.downgrade();
        hover.connect_enter(crate::guarded!("theme card: hover", move |_, _, _| {
            if let Some(art) = weak.upgrade() {
                art.imp().hovered.set(true);
                art.queue_draw();
            }
        }));
        let weak = art.downgrade();
        hover.connect_leave(crate::guarded!("theme card: leave", move |_| {
            if let Some(art) = weak.upgrade() {
                art.imp().hovered.set(false);
                art.queue_draw();
            }
        }));
        button.add_controller(hover);
        Self { button, art }
    }

    pub fn button(&self) -> &gtk::ToggleButton {
        &self.button
    }

    /// Shows the card on or off (its 2 px `accent` stroke and the check).
    pub fn set_selected(&self, on: bool) {
        let imp = self.art.imp();
        if imp.selected.replace(on) != on {
            self.art.queue_draw();
        }
    }

    /// Follows the UI scale (D-46: the card draws its 100 % px × the scale).
    pub fn set_scale(&self, scale: f64) {
        let imp = self.art.imp();
        if (imp.scale.replace(scale) - scale).abs() > f64::EPSILON {
            self.art.queue_resize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::palette::{DRAGONS_KEEP, SWAMP_DAY, SWAMP_NIGHT};

    /// S21's parts in each theme's own colors: Swamp Night's card is the
    /// frame's (its `surface`, the pill `record`, the panel's selected
    /// row, the meter `meter-green`, the lane the lily tint).
    #[test]
    fn the_preview_is_the_frames_in_its_themes_colors() {
        let parts = preview_parts(&SWAMP_NIGHT);
        assert_eq!(parts.len(), 19);
        assert_eq!(parts[1].rect, [128.0, 2.0, 18.0, 4.0]);
        assert_eq!(parts[1].color, SWAMP_NIGHT.record);
        assert_eq!(parts[4].color, SWAMP_NIGHT.selected);
        assert_eq!(parts[6].color, SWAMP_NIGHT.meter_green);
        let lane = parts[parts.len() - 3];
        assert_eq!(lane.rect, [0.0, 74.0, 124.0, 9.0]);
        assert_eq!(lane.color, SWAMP_NIGHT.take_lily_tint);
        // Dragon's Keep's meter is its green, not its accent (S21).
        assert_eq!(
            preview_parts(&DRAGONS_KEEP)[6].color,
            DRAGONS_KEEP.meter_green
        );
        assert_ne!(DRAGONS_KEEP.meter_green, DRAGONS_KEEP.accent);
        // Selected: 2 px `accent` and the check; else 1 px `border`.
        let on = card_colors(&SWAMP_DAY, true, false);
        assert_eq!((on.stroke, on.stroke_width), (SWAMP_DAY.accent, 2.0));
        let off = card_colors(&SWAMP_DAY, false, false);
        assert_eq!((off.stroke, off.stroke_width), (SWAMP_DAY.border, 1.0));
        assert_eq!(off.ground, SWAMP_DAY.surface);
        assert_eq!(off.name, SWAMP_DAY.text);
        assert_eq!(
            card_colors(&SWAMP_DAY, false, true).stroke,
            SWAMP_DAY.border_strong
        );
    }

    /// The canvas image is in the build, 2× its 72×40.
    #[test]
    fn the_canvas_image_is_two_times_its_size() {
        let png = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/art/theme-card-canvas.png");
        let bytes = std::fs::read(&png).expect("the card's canvas");
        assert_eq!(&bytes[1..4], b"PNG");
        let be = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        assert_eq!((be(16), be(20)), (144, 80));
    }
}
