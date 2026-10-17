//! C12 inspector (S06, S13): the selected layer's Position X/Y, Size W/H
//! with the aspect lock (default on, panel-local), Crop top/right/bottom/
//! left, Opacity (slider + field) and Fit (Contain/Cover). Canvas px for
//! the rect, source px for the crop.
//!
//! - Enter or leaving a field commits one `LayerSet{target: edit_target,
//!   layer, patch}` (`view::panel::inspector_commit_with`); an invalid value
//!   shows the field's invalid state and the helper text under its row,
//!   which go together (`Esc`, typing or the model's value clear both; a
//!   row's helper reads its newest error).
//! - The slider commits on release, the Fit segment at once.
//! - During a canvas drag or nudge burst the fields show
//!   `LocalUi.layer_drag` (WP4's transient geometry).
//! - An edit belongs to the scene and the layer it was typed for: when
//!   either changes under it (the mode, the selection, an agent's command)
//!   the fields show the new layer's values at once, even the focused one,
//!   and the edit is dropped, never committed to the new scene (R38).
//! - The Medium layout (320 px) lays the crop fields 2-up.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::State;
use qrec_core::model::Fit;
use qrec_core::protocol::command::SceneTarget;
use qrec_core::ui_normalize::{canvas_scene, edit_target};
use qrec_core::view::panel::{
    self, CommitOptions, INSPECTOR_EMPTY, Inspector, InspectorField, source_key,
};

use super::header::SectionHeader;
use crate::Ctx;
use crate::guarded;
use crate::store::local::{LayerDragPreview, LocalUi};
use crate::widgets::numeric::{self, NumericField};
use crate::widgets::scale_tooltip::{self, ValueTooltip};

/// The inspector section. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct InspectorView {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    header: SectionHeader,
    empty: gtk::Label,
    fields_box: gtk::Box,
    fields: BTreeMap<FieldKey, NumericField>,
    crop: gtk::Grid,
    crop_groups: Vec<gtk::Box>,
    helpers: BTreeMap<Row, gtk::Label>,
    /// The field whose error each row's helper reads.
    helper_owners: RefCell<BTreeMap<Row, InspectorField>>,
    lock: gtk::ToggleButton,
    opacity: gtk::Scale,
    _opacity_tooltip: ValueTooltip,
    fit: adw::ToggleGroup,
    setting: Cell<bool>,
    medium: Cell<bool>,
    shown: RefCell<Option<Inspector>>,
    /// What the fields edit: the scene (`edit_target`) and the layer.
    edits: RefCell<Option<(Option<SceneTarget>, qrec_core::LayerId)>>,
}

/// `InspectorField` as a map key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FieldKey(u8);

impl From<InspectorField> for FieldKey {
    fn from(field: InspectorField) -> Self {
        FieldKey(
            InspectorField::ALL
                .iter()
                .position(|f| *f == field)
                .unwrap_or(0) as u8,
        )
    }
}

/// The rows a helper text sits under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Row {
    Position,
    Size,
    Crop,
    Opacity,
}

/// The field's widget name (the screenshot cases find fields by name).
fn widget_name(field: InspectorField) -> &'static str {
    match field {
        InspectorField::X => "inspector-x",
        InspectorField::Y => "inspector-y",
        InspectorField::W => "inspector-w",
        InspectorField::H => "inspector-h",
        InspectorField::CropTop => "inspector-crop-top",
        InspectorField::CropRight => "inspector-crop-right",
        InspectorField::CropBottom => "inspector-crop-bottom",
        InspectorField::CropLeft => "inspector-crop-left",
        InspectorField::Opacity => "inspector-opacity",
    }
}

fn row_of(field: InspectorField) -> Row {
    match field {
        InspectorField::X | InspectorField::Y => Row::Position,
        InspectorField::W | InspectorField::H => Row::Size,
        InspectorField::Opacity => Row::Opacity,
        _ => Row::Crop,
    }
}

impl std::fmt::Debug for InspectorView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorView")
            .field("shown", &*self.inner.shown.borrow())
            .finish()
    }
}

/// A caption over a field (`caption` `muted`).
fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("t-caption");
    label.add_css_class("muted");
    label.add_css_class("field-caption");
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

/// A field under its caption.
fn group(text: &str, field: &gtk::Widget) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 4);
    group.add_css_class("field-group");
    group.set_hexpand(true);
    group.append(&caption(text));
    group.append(field);
    group
}

fn helper() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("field-helper");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_visible(false);
    label
}

impl InspectorView {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("inspector");
        root.set_widget_name("inspector");
        let header = SectionHeader::new("Inspector");
        let empty = gtk::Label::new(Some(INSPECTOR_EMPTY));
        empty.add_css_class("t-caption");
        empty.add_css_class("muted");
        empty.add_css_class("inspector-empty");
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        let mut fields = BTreeMap::new();
        for field in InspectorField::ALL {
            let f = NumericField::new(field.unit(), field.label());
            f.set_step(field.step());
            f.set_format(numeric::whole);
            f.widget().set_widget_name(widget_name(field));
            fields.insert(FieldKey::from(field), f);
        }
        let get = |f: InspectorField| fields[&FieldKey::from(f)].widget();
        let fields_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        fields_box.add_css_class("inspector-fields");
        let mut helpers = BTreeMap::new();
        // Position X / Y.
        let xy = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        xy.set_homogeneous(true);
        xy.append(&group(InspectorField::X.label(), &get(InspectorField::X)));
        xy.append(&group(InspectorField::Y.label(), &get(InspectorField::Y)));
        fields_box.append(&xy);
        let h = helper();
        fields_box.append(&h);
        helpers.insert(Row::Position, h);
        // Size W / lock / H.
        let wh = gtk::CenterBox::new();
        let w_group = group(InspectorField::W.label(), &get(InspectorField::W));
        let h_group = group(InspectorField::H.label(), &get(InspectorField::H));
        let lock = gtk::ToggleButton::new();
        lock.set_icon_name("insert-link-symbolic");
        lock.add_css_class("aspect-lock");
        lock.set_active(true);
        lock.set_focus_on_click(false);
        lock.update_property(&[gtk::accessible::Property::Label("Keep the aspect ratio")]);
        lock.set_tooltip_text(Some("Keep the aspect ratio"));
        let lock_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lock_box.add_css_class("aspect-lock-slot");
        lock_box.append(&lock);
        lock.set_valign(gtk::Align::End);
        lock_box.set_valign(gtk::Align::End);
        wh.set_start_widget(Some(&w_group));
        wh.set_center_widget(Some(&lock_box));
        wh.set_end_widget(Some(&h_group));
        fields_box.append(&wh);
        let h = helper();
        fields_box.append(&h);
        helpers.insert(Row::Size, h);
        // Crop T / R / B / L (2-up in Medium).
        let crop = gtk::Grid::new();
        crop.set_column_spacing(8);
        crop.set_row_spacing(4);
        crop.set_column_homogeneous(true);
        let crop_groups: Vec<gtk::Box> = [
            InspectorField::CropTop,
            InspectorField::CropRight,
            InspectorField::CropBottom,
            InspectorField::CropLeft,
        ]
        .into_iter()
        .map(|f| group(f.label(), &get(f)))
        .collect();
        fields_box.append(&crop);
        let h = helper();
        fields_box.append(&h);
        helpers.insert(Row::Crop, h);
        // Opacity: slider + field.
        let opacity_group = gtk::Box::new(gtk::Orientation::Vertical, 4);
        opacity_group.append(&caption(InspectorField::Opacity.label()));
        let opacity_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        opacity.set_draw_value(false);
        opacity.set_hexpand(true);
        opacity.set_valign(gtk::Align::Center);
        opacity.set_focus_on_click(false);
        opacity.update_property(&[gtk::accessible::Property::Label("Opacity")]);
        let opacity_field = get(InspectorField::Opacity);
        opacity_field.add_css_class("opacity-field");
        opacity_row.append(&opacity);
        opacity_row.append(&opacity_field);
        opacity_group.append(&opacity_row);
        fields_box.append(&opacity_group);
        let h = helper();
        fields_box.append(&h);
        helpers.insert(Row::Opacity, h);
        // Fit.
        let fit_group = gtk::Box::new(gtk::Orientation::Vertical, 4);
        fit_group.append(&caption("Fit"));
        let fit = adw::ToggleGroup::new();
        fit.add_css_class("fit-toggle");
        fit.set_halign(gtk::Align::Start);
        for (name, label) in [("contain", "Contain"), ("cover", "Cover")] {
            let toggle = adw::Toggle::new();
            toggle.set_name(Some(name));
            toggle.set_label(Some(label));
            fit.add(toggle);
        }
        fit.upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Fit")]);
        fit_group.append(&fit);
        fields_box.append(&fit_group);
        root.append(&header.widget());
        root.append(&empty);
        root.append(&fields_box);
        let store = ctx.store.clone();
        let tooltip = scale_tooltip::attach(
            &opacity,
            |v| format!("{} %", v.round()),
            move |value| {
                if let Some(cmd) = panel::opacity_commit(&store.state(), value) {
                    store.dispatch(cmd);
                }
            },
        );
        let view = Self {
            inner: Rc::new(Inner {
                ctx: ctx.clone(),
                root,
                header,
                empty,
                fields_box,
                fields,
                crop,
                crop_groups,
                helpers,
                helper_owners: RefCell::new(BTreeMap::new()),
                lock,
                opacity,
                _opacity_tooltip: tooltip,
                fit,
                setting: Cell::new(false),
                medium: Cell::new(true),
                shown: RefCell::new(None),
                edits: RefCell::new(None),
            }),
        };
        view.lay_out_crop(false);
        view.wire();
        view
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// The field of `field` (tests, focus).
    pub fn field(&self, field: InspectorField) -> NumericField {
        self.inner.fields[&FieldKey::from(field)].clone()
    }

    /// The crop fields 4-up (Wide) or 2-up (Medium, G§3.15).
    pub fn lay_out_crop(&self, medium: bool) {
        let inner = &self.inner;
        if inner.medium.replace(medium) == medium {
            return;
        }
        for group in &inner.crop_groups {
            if group.parent().is_some() {
                inner.crop.remove(group);
            }
        }
        for (i, group) in inner.crop_groups.iter().enumerate() {
            let (column, row) = if medium {
                ((i % 2) as i32, (i / 2) as i32)
            } else {
                (i as i32, 0)
            };
            inner.crop.attach(group, column, row, 1, 1);
        }
    }

    fn wire(&self) {
        let inner = &self.inner;
        for field in InspectorField::ALL {
            let weak = Rc::downgrade(inner);
            inner.fields[&FieldKey::from(field)].on_commit(move |text| {
                if let Some(inner) = weak.upgrade() {
                    InspectorView { inner }.commit(field, text);
                }
            });
            let weak = Rc::downgrade(inner);
            inner.fields[&FieldKey::from(field)].on_error(move |error| {
                if let Some(inner) = weak.upgrade() {
                    InspectorView { inner }.show_error(field, error);
                }
            });
        }
        // The slider moves the field along; it commits on release.
        let weak = Rc::downgrade(inner);
        inner
            .opacity
            .connect_value_changed(guarded!("inspector: opacity", move |scale| {
                if let Some(inner) = weak.upgrade()
                    && !inner.setting.get()
                {
                    inner.fields[&FieldKey::from(InspectorField::Opacity)]
                        .set_value(&numeric::whole(scale.value()));
                }
            }));
        let weak = Rc::downgrade(inner);
        inner
            .fit
            .connect_active_name_notify(guarded!("inspector: fit", move |group| {
                let Some(inner) = weak.upgrade() else { return };
                if inner.setting.get() {
                    return;
                }
                let fit = match group.active_name().as_deref() {
                    Some("contain") => Fit::Contain,
                    Some("cover") => Fit::Cover,
                    _ => return,
                };
                let store = inner.ctx.store.clone();
                if let Some(cmd) = panel::fit_commit(&store.state(), fit) {
                    store.dispatch(cmd);
                }
            }));
    }

    fn commit(&self, field: InspectorField, text: &str) {
        let inner = &self.inner;
        let store = &inner.ctx.store;
        let state = store.state();
        let options = CommitOptions {
            aspect_locked: inner.lock.is_active(),
            source_size: live_size(&state, &store.local()),
        };
        let input = &inner.fields[&FieldKey::from(field)];
        match panel::inspector_commit_with(&state, field, text, &options) {
            Ok(cmd) => {
                input.set_error(None);
                match cmd {
                    Some(cmd) => store.dispatch(cmd),
                    None => input.revert(),
                }
            }
            Err(why) => input.set_error(Some(&why)),
        }
    }

    /// `field`'s error changed: its row's helper reads it, or, when it
    /// went, another error of the row, else nothing (reverting W never
    /// hides H's error).
    fn show_error(&self, field: InspectorField, error: Option<&str>) {
        let inner = &self.inner;
        let row = row_of(field);
        let helper = &inner.helpers[&row];
        let mut owners = inner.helper_owners.borrow_mut();
        if let Some(why) = error {
            owners.insert(row, field);
            helper.set_text(why);
            helper.set_visible(true);
            return;
        }
        if owners.get(&row) != Some(&field) {
            return;
        }
        let other = InspectorField::ALL
            .into_iter()
            .filter(|f| *f != field && row_of(*f) == row)
            .find_map(|f| Some((f, inner.fields[&FieldKey::from(f)].error()?)));
        match other {
            Some((f, why)) => {
                owners.insert(row, f);
                helper.set_text(&why);
            }
            None => {
                owners.remove(&row);
                helper.set_visible(false);
            }
        }
    }

    /// Every field without its error (and so every helper hidden).
    fn clear_errors(&self) {
        for field in self.inner.fields.values() {
            field.set_error(None);
        }
    }

    /// Shows the inspector of `state` (with `local`'s drag preview).
    pub fn update(&self, state: &State, local: &LocalUi) {
        let inner = &self.inner;
        // S01a: an empty scene shows no inspector at all.
        inner.root.set_visible(panel::inspector_shown(state));
        self.lay_out_crop(local.layout.width == crate::store::local::LayoutWidth::Medium);
        let shown = panel::inspector(state).map(|i| with_drag(i, state, local.layer_drag.as_ref()));
        inner
            .header
            .set_caption(&panel::inspector_heading(state).unwrap_or_default());
        let Some(insp) = shown else {
            inner.empty.set_visible(true);
            inner.fields_box.set_visible(false);
            inner
                .empty
                .set_sensitive(qrec_core::view::enabled(state, &probe()).is_ok());
            *inner.shown.borrow_mut() = None;
            *inner.edits.borrow_mut() = None;
            self.clear_errors();
            return;
        };
        inner.empty.set_visible(false);
        inner.fields_box.set_visible(true);
        // Another scene or layer: an edit in progress was for what went. Edits
        // stopped (D-43: a recording): it can't land, so it ends unsaved
        // rather than be refused when the disabled field lets go (R46).
        let edits = (edit_target(&state.project, &state.ui), insp.layer.clone());
        let layer_changed = inner.edits.borrow().as_ref() != Some(&edits);
        *inner.edits.borrow_mut() = Some(edits);
        for field in InspectorField::ALL {
            let input = &inner.fields[&FieldKey::from(field)];
            if layer_changed || !insp.enabled {
                input.reset(&field.text(&insp));
            } else {
                input.set_value(&field.text(&insp));
            }
        }
        inner.setting.set(true);
        if !inner.opacity.has_focus() || layer_changed {
            inner.opacity.set_value(f64::from(insp.opacity_percent));
        }
        inner.fit.set_active_name(Some(match insp.fit {
            Fit::Contain => "contain",
            Fit::Cover => "cover",
        }));
        inner.setting.set(false);
        inner.fields_box.set_sensitive(insp.enabled);
        *inner.shown.borrow_mut() = Some(insp);
    }
}

/// A `LayerSet` probe for the gates (the empty state dims with them).
fn probe() -> qrec_core::Command {
    qrec_core::Command::LayerSet {
        target: SceneTarget::Project,
        layer: qrec_core::LayerId::numbered(1),
        patch: Default::default(),
    }
}

/// The inspector with a canvas drag's transient geometry, when the drag
/// moves this layer of the scene being edited.
pub fn with_drag(mut insp: Inspector, state: &State, drag: Option<&LayerDragPreview>) -> Inspector {
    let Some(drag) = drag else {
        return insp;
    };
    let target = edit_target(&state.project, &state.ui);
    if drag.layer != insp.layer || target.as_ref() != Some(&drag.target) {
        return insp;
    }
    insp.x = drag.rect.x;
    insp.y = drag.rect.y;
    insp.w = drag.rect.w;
    insp.h = drag.rect.h;
    insp.crop = [
        drag.crop.top,
        drag.crop.right,
        drag.crop.bottom,
        drag.crop.left,
    ];
    insp
}

/// The selected layer's live source size (Live mode), for the crop check.
fn live_size(state: &State, local: &LocalUi) -> Option<(u32, u32)> {
    if edit_target(&state.project, &state.ui) != Some(SceneTarget::Project) {
        return None;
    }
    let id = state.ui.selected_layer.as_ref()?;
    let layer = canvas_scene(&state.project, &state.ui).layer(id)?;
    let key = source_key(&layer.source);
    local
        .live_status
        .iter()
        .find(|s| s.source.name() == key)
        .and_then(|s| s.size)
}

#[cfg(test)]
mod tests {
    use qrec_core::model::{Crop, Rect};
    use qrec_core::state::CanvasMode;
    use qrec_core::{ClipId, LayerId, TakeId, testkit};

    use super::*;

    #[test]
    fn the_fields_follow_a_canvas_drag_of_their_layer() {
        let mut state = testkit::design_state();
        state.ui.selected_layer = Some(LayerId::numbered(2));
        let insp = panel::inspector(&state).unwrap();
        let drag = LayerDragPreview {
            target: SceneTarget::Project,
            layer: LayerId::numbered(2),
            rect: Rect::new(1280, 760, 384, 288),
            crop: Crop::default(),
        };
        // S13: X reads 1280 while the canvas drags.
        assert_eq!(with_drag(insp.clone(), &state, Some(&drag)).x, 1280);
        assert_eq!(with_drag(insp.clone(), &state, None).x, 1500);
        // Another layer's drag, or another scene's, changes nothing.
        let other = LayerDragPreview {
            layer: LayerId::numbered(1),
            ..drag.clone()
        };
        assert_eq!(with_drag(insp.clone(), &state, Some(&other)).x, 1500);
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.selection = vec!["c2d0".parse::<ClipId>().unwrap()];
        let insp = panel::inspector(&state).unwrap();
        assert_eq!(with_drag(insp.clone(), &state, Some(&drag)).x, 1500);
        let take = LayerDragPreview {
            target: SceneTarget::Take(TakeId::numbered(5)),
            ..drag
        };
        assert_eq!(with_drag(insp, &state, Some(&take)).x, 1280);
    }
}
