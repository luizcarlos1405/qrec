//! What the timeline draws (C15–C18): the ruler, the canvas (clips and
//! their filmstrips, lane segments and waveforms, hatches, the ghost, the
//! trim and move previews, carets) and the overlay (the playhead, the
//! scrub flag, the snap guide). All of it from `view::timeline` and
//! `view::ruler`, in 100 % px (the caller scales by the UI scale), colors
//! from the theme's `Palette`.

use std::sync::Arc;

use gtk::gsk;
use gtk::prelude::*;
use qrec_core::view::AutocutPreview;
use qrec_core::view::ruler::{self, TickKind};
use qrec_core::view::timeline::{
    self as tl, ClipBox, ClipLabel, Ghost, Lane, LaneSegment, MovePreview, Rows, STRIPE_PX,
    SilenceSpan, TagAnchor, ThumbImage, TrimEdge, TrimPreview, Viewport, WAVE_HALF_PX,
};
use qrec_core::{Frame, State};
use qrec_shell::media::preview::PeakLevels;

use super::paint::{self, Role, Texts, rect};
use crate::media::peaks::PeakCache;
use crate::media::thumbs::ThumbCache;
use crate::theme::Palette;

/// What a canvas frame shows (built by the timeline for each draw).
pub struct Scene<'a> {
    pub widget: &'a gtk::Widget,
    pub texts: &'a Texts,
    pub palette: &'a Palette,
    pub state: &'a State,
    pub view: Viewport,
    pub rows: Rows,
    pub lanes: &'a [Lane],
    pub thumbs: Option<&'a ThumbCache>,
    pub peaks: Option<&'a PeakCache>,
    /// The thumbnails' height in device px (72 × the UI scale × the scale
    /// factor: `control::thumb_px`).
    pub thumb_px: u32,
    /// The animation clock (spinners, the saving ghost's stripes).
    pub now_ms: u64,
    /// Reduced motion (D-30): the saving ghost's stripes hold still
    /// (DESIGN Progress: static stripes; spinners keep turning).
    pub still: bool,
    /// The clips as drawn: the timeline, or a preview's layout.
    pub boxes: Vec<ClipBox>,
    pub ghost: Option<Ghost>,
    /// The live waveform under the ghost's columns in view.
    pub ghost_waves: Option<GhostWaves>,
    pub silences: &'a [SilenceSpan],
    pub autocut: Option<Arc<AutocutPreview>>,
    pub trim: Option<TrimPreview>,
    /// A move drag and the lifted copy's left x.
    pub moving: Option<(MovePreview, Vec<ClipBox>)>,
    /// A bin drop's caret.
    pub drop_caret: Option<f64>,
}

/// The live waveform under the ghost's columns in view (G§4.5): per lane,
/// an amplitude per px in screen order from `x0`.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostWaves {
    pub x0: f64,
    pub lanes: Vec<Vec<f32>>,
}

impl Scene<'_> {
    fn width(&self) -> f64 {
        self.view.width
    }

    fn lane_index(&self, track: &qrec_core::TrackId) -> Option<usize> {
        self.lanes.iter().position(|l| &l.track == track)
    }

    fn content_height(&self) -> f64 {
        self.rows.content_height(self.lanes.len())
    }

    fn visible(&self, x: f64, w: f64) -> bool {
        x + w > 0.0 && x < self.width()
    }
}

// ------------------------------------------------------------- the canvas

/// The rows: lanes, clips, hatches, the ghost, previews and carets.
pub fn canvas(s: &gtk::Snapshot, sc: &Scene<'_>, height: f64) {
    let p = sc.palette;
    let w = sc.width();
    s.append_color(&p.bg, &rect(0.0, 0.0, w, height));
    s.push_clip(&rect(0.0, 0.0, w, height));
    for (i, lane) in sc.lanes.iter().enumerate() {
        let segments = tl::lane_segments(sc.state, &sc.view, &sc.boxes, lane);
        for (seg, b) in segments.iter().zip(&sc.boxes) {
            if sc.visible(seg.x, seg.w) {
                segment(s, sc, seg, b.opacity, sc.rows.lane_y(i));
            }
        }
    }
    for b in &sc.boxes {
        if sc.visible(b.x, b.w) {
            clip(s, sc, b, 0.0);
        }
    }
    silences(s, sc);
    if let Some(preview) = &sc.autocut {
        autocut(s, sc, preview);
    }
    if let Some(trim) = &sc.trim {
        trim_preview(s, sc, trim);
    }
    if let Some(ghost) = &sc.ghost {
        ghost_clip(s, sc, ghost);
    }
    if let Some((preview, lifted)) = &sc.moving {
        caret(s, sc, preview.caret_x);
        if let Some(tag) = &preview.ripple {
            ripple(s, sc, tag);
        }
        lifted_copy(s, sc, lifted);
    }
    if let Some(x) = sc.drop_caret {
        caret(s, sc, x);
    }
    s.pop();
}

/// One clip on the video row, `dy` px down (the lifted copy is 4 px up).
fn clip(s: &gtk::Snapshot, sc: &Scene<'_>, b: &ClipBox, dy: f64) {
    let p = sc.palette;
    let rows = sc.rows;
    let (x, w) = (b.x, b.w);
    let r = rect(x, dy, w, rows.video);
    if b.opacity < 1.0 {
        s.push_opacity(b.opacity);
    }
    s.append_color(&p.raised, &r);
    s.append_color(&p.take[b.color % 8], &rect(x, dy, w, STRIPE_PX));
    let film = rect(x, dy + STRIPE_PX, w, rows.filmstrip());
    if b.missing {
        paint::hatch(s, &film, &p.hatch_stripe, 0.0);
        if w >= 24.0 {
            let ix = x.max(0.0) + (w.min(sc.width() - x.max(0.0)) - 16.0).max(0.0) / 2.0;
            paint::icon(
                s,
                sc.widget,
                "dialog-warning-symbolic",
                (ix, dy + STRIPE_PX + (rows.filmstrip() - 16.0) / 2.0),
                16,
                &p.warning_text,
            );
        }
    } else if !matches!(b.label, ClipLabel::Sliver) {
        filmstrip(s, sc, b, dy);
    }
    if b.recorded_over {
        paint::hatch(s, &r, &p.muted_40, 0.0);
    }
    if matches!(b.label, ClipLabel::Sliver) {
        s.append_color(&p.text_35, &rect(x, dy, 1.0, rows.video));
    }
    // The 1 px divider at the right edge.
    if w >= 2.0 {
        s.append_color(&p.bg, &rect(x + w - 1.0, dy, 1.0, rows.video));
    }
    if b.hovered && !b.selected {
        paint::inner_stroke(s, &r, 1.0, &p.text_35);
    }
    if b.selected {
        paint::inner_stroke(s, &r, 2.0, &p.accent);
        if w >= 12.0 {
            let hy = dy + (rows.video - 28.0) / 2.0;
            s.append_color(&p.accent, &rect(x, hy, 4.0, 28.0));
            s.append_color(&p.accent, &rect(x + w - 4.0, hy, 4.0, 28.0));
        }
    }
    label(s, sc, b, dy);
    if let Some(edge) = b.trimming {
        let bx = match edge {
            TrimEdge::Start => x,
            TrimEdge::End => x + w - 6.0,
        };
        s.append_color(&p.accent, &rect(bx, dy, 6.0, rows.video));
    }
    if b.opacity < 1.0 {
        s.pop();
    }
}

/// Where a clip's label plate starts: 6 px in, kept on screen.
fn label_x(b: &ClipBox) -> f64 {
    b.x.max(0.0) + 6.0
}

/// The take label plate (C16) at (6, 10), with a spinner while autocut
/// analyzes the clip.
fn label(s: &gtk::Snapshot, sc: &Scene<'_>, b: &ClipBox, dy: f64) {
    let Some(text) = b.label.text() else {
        return;
    };
    let p = sc.palette;
    let lead = if b.analyzing { 16.0 } else { 0.0 };
    let x = label_x(b);
    let width = paint::plate_width(sc.widget, sc.texts, Role::CaptionStrong, text, lead);
    if x + width > b.x + b.w - 2.0 {
        return;
    }
    paint::plate(
        s,
        sc.widget,
        sc.texts,
        Role::CaptionStrong,
        text,
        (x, dy + 10.0),
        lead,
        &p.video_scrim,
        &p.video_text,
    );
    if b.analyzing {
        paint::spinner(
            s,
            (
                x + paint::PLATE_PAD,
                dy + 10.0 + (paint::PLATE_H - 12.0) / 2.0,
            ),
            12.0,
            sc.now_ms,
            &p.video_text,
        );
    }
}

/// The visible filmstrip tiles of `b` (C16): textures cover-fit into their
/// 128 px tiles, center-cropped frames at frame level, `raised` while one
/// loads.
fn filmstrip(s: &gtk::Snapshot, sc: &Scene<'_>, b: &ClipBox, dy: f64) {
    let Some(thumbs) = sc.thumbs else {
        return;
    };
    let rows = sc.rows;
    let fh = rows.filmstrip();
    let fy = dy + STRIPE_PX;
    let left = b.x.max(0.0);
    let right = (b.x + b.w).min(sc.width());
    s.push_clip(&rect(left, fy, right - left, fh));
    for tile in tl::thumb_tiles(sc.state, &sc.view, b) {
        let (texture, frame_level) = match &tile.image {
            ThumbImage::Strip { file, second } => {
                (thumbs.strip_image(file, *second, sc.thumb_px), false)
            }
            ThumbImage::Frame {
                file,
                recorded,
                second,
            } => match thumbs.frame(file, *recorded, sc.thumb_px) {
                Some(t) => (Some(t), true),
                None => (thumbs.strip_image(file, *second, sc.thumb_px), true),
            },
        };
        let Some(texture) = texture else {
            continue;
        };
        let aspect = f64::from(texture.width()) / f64::from(texture.height().max(1));
        let (dw, dh, dx) = if frame_level {
            let dw = fh * aspect;
            (dw, fh, tile.x + (tile.w - dw) / 2.0)
        } else {
            let dh = fh.max(tl::TILE_PX / aspect);
            let dw = dh * aspect;
            (dw, dh, tile.x + (tl::TILE_PX - dw) / 2.0)
        };
        s.push_clip(&rect(tile.x, fy, tile.w, fh));
        s.append_texture(&texture, &rect(dx, fy + (fh - dh) / 2.0, dw, dh));
        s.pop();
    }
    s.pop();
}

/// A lane segment (C17): the take tint, the selection tint, the waveform,
/// the divider.
fn segment(s: &gtk::Snapshot, sc: &Scene<'_>, seg: &LaneSegment, opacity: f64, y: f64) {
    let p = sc.palette;
    let h = sc.rows.lane;
    let r = rect(seg.x, y, seg.w, h);
    if opacity < 1.0 {
        s.push_opacity(opacity);
    }
    s.append_color(&p.take_tint[seg.color % 8], &r);
    if seg.selected {
        s.append_color(&p.selected, &r);
    }
    if let (Some(file), Some(peaks)) = (&seg.file, sc.peaks)
        && let Some(levels) = peaks.peaks(file)
    {
        let x0 = seg.x.max(0.0).floor();
        let x1 = (seg.x + seg.w).min(sc.width());
        let amps = amplitudes(&levels, seg, x0, x1);
        let ink = if seg.dimmed { &p.wave_dim } else { &p.wave };
        paint::waveform(
            s,
            x0,
            (seg.x, seg.x + seg.w),
            y + h / 2.0,
            WAVE_HALF_PX.min(h / 2.0 - 2.0),
            &amps,
            ink,
        );
    }
    if seg.w >= 2.0 {
        s.append_color(&p.bg, &rect(seg.x + seg.w - 1.0, y, 1.0, h));
    }
    if opacity < 1.0 {
        s.pop();
    }
}

/// The waveform's amplitude (0..1) per px column in `[x0, x1)`, from the
/// coarsest peak level with at least one bucket per px.
pub fn amplitudes(levels: &PeakLevels, seg: &LaneSegment, x0: f64, x1: f64) -> Vec<f32> {
    let (bucket_ms, buckets) = levels.level(seg.ms_per_px);
    let bucket_ms = f64::from(bucket_ms.max(1));
    let n = (x1 - x0).ceil().max(0.0) as usize;
    let len = buckets.len();
    let mut out = Vec::with_capacity(n);
    for c in 0..n {
        let x = x0 + c as f64;
        let ms0 = seg.file_ms_at_x + (x - seg.x) * seg.ms_per_px;
        let ms1 = ms0 + seg.ms_per_px;
        if ms1 <= 0.0 || len == 0 {
            out.push(0.0);
            continue;
        }
        let b0 = (ms0 / bucket_ms).floor().max(0.0) as usize;
        if b0 >= len {
            out.push(0.0);
            continue;
        }
        let b1 = ((ms1 / bucket_ms).ceil() as usize).clamp(b0 + 1, len);
        let peak = buckets[b0..b1]
            .iter()
            .map(|(lo, hi)| i32::from(*lo).abs().max(i32::from(*hi).abs()))
            .max()
            .unwrap_or(0);
        out.push((peak as f32 / 32768.0).min(1.0));
    }
    out
}

/// The silences on the autocut track's lane (S08): the hatch with its
/// warning edges.
fn silences(s: &gtk::Snapshot, sc: &Scene<'_>) {
    let p = sc.palette;
    for span in sc.silences {
        let Some(i) = sc.lane_index(&span.track) else {
            continue;
        };
        let (x0, x1) = (sc.view.x_of_frame(span.from), sc.view.x_of_frame(span.to));
        if !sc.visible(x0, x1 - x0) {
            continue;
        }
        let r = rect(x0, sc.rows.lane_y(i), x1 - x0, sc.rows.lane);
        paint::hatch(s, &r, &p.hatch_stripe, 0.0);
        paint::edges(s, &r, &p.warning);
    }
}

/// The autocut page's preview (S12, `LocalUi.autocut_preview`, WP5's):
/// each region hatched in `warning` at full strength on its lane, with its
/// warning edges, the same range dimmed (`bg-55`) on the video row and the
/// other lanes, and its "−1.6 s" tag 22 px above the lane.
fn autocut(s: &gtk::Snapshot, sc: &Scene<'_>, preview: &AutocutPreview) {
    let p = sc.palette;
    let rows = sc.rows;
    for span in &preview.spans {
        let Some(lane) = sc.lane_index(&span.track) else {
            continue;
        };
        let (x0, x1) = (sc.view.x_of_frame(span.from), sc.view.x_of_frame(span.to));
        if !sc.visible(x0, x1 - x0) {
            continue;
        }
        let w = x1 - x0;
        s.append_color(&p.bg_55, &rect(x0, 0.0, w, rows.video));
        for other in (0..sc.lanes.len()).filter(|&i| i != lane) {
            s.append_color(&p.bg_55, &rect(x0, rows.lane_y(other), w, rows.lane));
        }
        let r = rect(x0, rows.lane_y(lane), w, rows.lane);
        paint::hatch(s, &r, &p.warning, 0.0);
        paint::edges(s, &r, &p.warning);
        if !span.tag.is_empty() {
            paint::plate(
                s,
                sc.widget,
                sc.texts,
                Role::MonoSm,
                &span.tag,
                (x0 + 2.0, rows.lane_y(lane) - 22.0),
                0.0,
                &p.video_scrim,
                &p.warning_text,
            );
        }
    }
}

/// The trim preview (S10): the trimmed (or still available) media as a
/// dimmed filmstrip in a dashed `accent` outline, its tag, the lanes under
/// it dimmed, and the ripple tag.
fn trim_preview(s: &gtk::Snapshot, sc: &Scene<'_>, t: &TrimPreview) {
    let p = sc.palette;
    let rows = sc.rows;
    if let Some((gx, gw)) = t.ghost
        && let Some(b) = sc.boxes.iter().find(|b| b.clip == t.clip)
    {
        let clip = sc.state.project.clip(&t.clip);
        // The ghost's first take frame, at its left edge: the old `in` (a
        // shortened start), the take's first frame (a start extended), the
        // new `out` (an end either way).
        let in_frame = match (t.edge, clip) {
            (TrimEdge::Start, Some(c)) if t.d > 0 => c.in_,
            (TrimEdge::Start, _) => 0,
            (TrimEdge::End, Some(c)) => (c.out as i64 + t.d).max(0) as Frame,
            (TrimEdge::End, None) => b.in_frame,
        };
        let virtual_box = ClipBox {
            x: gx,
            w: gw,
            in_frame,
            label: ClipLabel::None,
            selected: false,
            hovered: false,
            trimming: None,
            recorded_over: false,
            missing: b.missing,
            opacity: 1.0,
            ..b.clone()
        };
        let r = rect(gx, 0.0, gw, rows.video);
        s.push_opacity(0.4);
        s.append_color(&p.raised, &r);
        if !b.missing {
            filmstrip(s, sc, &virtual_box, 0.0);
        }
        s.pop();
        paint::dashed(s, &r, 1.5, &[6.0, 4.0], &p.accent);
        for i in 0..sc.lanes.len() {
            s.append_color(&p.bg_55, &rect(gx, rows.lane_y(i), gw, rows.lane));
        }
    }
    if t.d != 0 {
        paint::plate(
            s,
            sc.widget,
            sc.texts,
            Role::MonoSm,
            &t.tag,
            (t.tag_x, rows.video - 4.0 - paint::PLATE_H),
            0.0,
            &p.video_scrim,
            &p.video_text,
        );
    }
    if let Some(tag) = &t.ripple {
        ripple(s, sc, tag);
    }
}

/// A ripple tag ("← 00:00:00:29"): after a clip's label (S10) or centered
/// at the video row's lower edge (S11).
fn ripple(s: &gtk::Snapshot, sc: &Scene<'_>, tag: &tl::RippleTag) {
    let p = sc.palette;
    let width = paint::plate_width(sc.widget, sc.texts, Role::MonoSm, &tag.text, 0.0);
    let (x, y) = match &tag.anchor {
        TagAnchor::Center { x } => (x - width / 2.0, sc.rows.video - 4.0 - paint::PLATE_H),
        TagAnchor::AfterLabel { clip } => {
            let Some(b) = sc.boxes.iter().find(|b| &b.clip == clip) else {
                return;
            };
            let after = match b.label.text() {
                Some(text) => {
                    label_x(b)
                        + paint::plate_width(sc.widget, sc.texts, Role::CaptionStrong, text, 0.0)
                        + 6.0
                }
                None => label_x(b),
            };
            (after, 10.0)
        }
    };
    paint::plate(
        s,
        sc.widget,
        sc.texts,
        Role::MonoSm,
        &tag.text,
        (x, y),
        0.0,
        &p.video_scrim,
        &p.video_text,
    );
}

/// The saving ghost's stripes' phase at `now_ms` (px along x): one stripe
/// step every 1.2 s, like the indeterminate bar; still (0) under reduced
/// motion (DESIGN Progress: static stripes).
pub fn saving_phase(now_ms: u64, still: bool) -> f32 {
    if still {
        0.0
    } else {
        (now_ms % 1200) as f32 / 1200.0 * paint::hatch_step()
    }
}

/// The ghost of the take being recorded (S04, S04b, S05).
fn ghost_clip(s: &gtk::Snapshot, sc: &Scene<'_>, g: &Ghost) {
    let p = sc.palette;
    let rows = sc.rows;
    if !sc.visible(g.x, g.w.max(1.0)) {
        return;
    }
    let r = rect(g.x, 0.0, g.w, rows.video);
    s.append_color(&p.ghost_fill, &r);
    s.append_color(&p.take[g.color % 8], &rect(g.x, 0.0, g.w, STRIPE_PX));
    if g.saving {
        paint::hatch(s, &r, &p.record_25, saving_phase(sc.now_ms, sc.still));
    }
    paint::dashed(s, &r, 1.5, &[6.0, 4.0], &p.record);
    for (i, lane) in sc.lanes.iter().enumerate() {
        let y = rows.lane_y(i);
        let lr = rect(g.x, y, g.w, rows.lane);
        s.append_color(&p.ghost_fill, &lr);
        if let Some(waves) = &sc.ghost_waves
            && let Some(amps) = waves.lanes.get(i)
            && !amps.is_empty()
        {
            let ink = if lane.dimmed { &p.wave_dim } else { &p.wave };
            paint::waveform(
                s,
                waves.x0,
                (g.x.max(0.0), (g.x + g.w).min(sc.width())),
                y + rows.lane / 2.0,
                WAVE_HALF_PX.min(rows.lane / 2.0 - 2.0),
                amps,
                ink,
            );
        }
    }
    // The plate: "● Take 10 · recording" (the dot in `record`), a second
    // line for a record-over, or the spinner and "Take 10 · saving…".
    let (text, dot) = match g.plate.strip_prefix("\u{25cf} ") {
        Some(rest) => (rest, true),
        None => (g.plate.as_str(), false),
    };
    let lead = if dot || g.saving { 14.0 } else { 0.0 };
    let x = g.x.max(0.0) + 6.0;
    let first = paint::plate_width(sc.widget, sc.texts, Role::CaptionStrong, text, lead);
    let second = g
        .over
        .as_deref()
        .map(|o| paint::plate_width(sc.widget, sc.texts, Role::Caption, o, lead))
        .unwrap_or(0.0);
    let w = first.max(second);
    let h = if g.over.is_some() {
        paint::PLATE_H + 16.0
    } else {
        paint::PLATE_H
    };
    paint::rounded(s, &rect(x, 10.0, w, h), 4.0, &p.video_scrim);
    let tx = x + paint::PLATE_PAD + lead;
    sc.texts.draw(
        s,
        sc.widget,
        Role::CaptionStrong,
        text,
        (tx, 11.0),
        &p.video_text,
    );
    if let Some(over) = &g.over {
        sc.texts
            .draw(s, sc.widget, Role::Caption, over, (tx, 27.0), &p.video_text);
    }
    if dot {
        paint::rounded(
            s,
            &rect(
                x + paint::PLATE_PAD + 1.0,
                10.0 + (paint::PLATE_H - 8.0) / 2.0,
                8.0,
                8.0,
            ),
            4.0,
            &p.record,
        );
    } else if g.saving {
        paint::spinner(
            s,
            (x + paint::PLATE_PAD, 10.0 + (paint::PLATE_H - 12.0) / 2.0),
            12.0,
            sc.now_ms,
            &p.video_text,
        );
    }
}

/// The insertion caret (C18): 2 px `caret` across the rows with 8×6
/// triangles pointing in at both ends.
fn caret(s: &gtk::Snapshot, sc: &Scene<'_>, x: f64) {
    let p = sc.palette;
    let bottom = sc.content_height();
    s.append_color(&p.caret, &rect(x - 1.0, 0.0, 2.0, bottom));
    paint::triangle(s, (x, 0.0), 8.0, 6.0, true, &p.caret);
    paint::triangle(s, (x, bottom - 6.0), 8.0, 6.0, false, &p.caret);
}

/// The lifted copy of a move (S11): 85 %, e1, 4 px up, over the video row
/// and the lanes.
fn lifted_copy(s: &gtk::Snapshot, sc: &Scene<'_>, boxes: &[ClipBox]) {
    let (Some(first), Some(last)) = (boxes.first(), boxes.last()) else {
        return;
    };
    let p = sc.palette;
    let (x0, x1) = (first.x, last.x + last.w);
    let bottom = sc.content_height();
    let shadow = gsk::RoundedRect::from_rect(rect(x0, -4.0, x1 - x0, bottom), 2.0);
    s.append_outset_shadow(&shadow, &p.shadow_e1, 0.0, 2.0, 0.0, 6.0);
    s.push_opacity(0.85);
    for (i, lane) in sc.lanes.iter().enumerate() {
        let segments = tl::lane_segments(sc.state, &sc.view, boxes, lane);
        for (seg, b) in segments.iter().zip(boxes) {
            segment(s, sc, seg, b.opacity, sc.rows.lane_y(i) - 4.0);
        }
    }
    for b in boxes {
        clip(s, sc, b, -4.0);
    }
    s.pop();
}

// ------------------------------------------------------------- the ruler

/// The ruler's time part (C15): ticks rising from the bottom, labels 4 px
/// right of their tick, frame cells at frame level with the playhead's
/// band.
#[allow(clippy::too_many_arguments)]
pub fn ruler(
    s: &gtk::Snapshot,
    widget: &gtk::Widget,
    texts: &Texts,
    p: &Palette,
    view: &Viewport,
    state: &State,
    playhead: Frame,
    height: f64,
) {
    let w = view.width;
    s.append_color(&p.surface, &rect(0.0, 0.0, w, height));
    s.push_clip(&rect(0.0, 0.0, w, height));
    let fps = state.project.canvas.fps;
    let total = qrec_core::timeline::total_frames(&state.project.timeline.clips);
    if let Some((x, cw)) = ruler::current_cell(view, playhead) {
        s.append_color(&p.accent_2_20, &rect(x, 0.0, cw, height));
    }
    for tick in ruler::ticks(view, fps, total) {
        let (tick_h, ink) = match tick.kind {
            TickKind::Major => (10.0, &p.muted),
            TickKind::Minor | TickKind::Frame => (5.0, &p.muted),
            TickKind::Second => (height, &p.muted),
        };
        s.append_color(ink, &rect(tick.x, height - tick_h, 1.0, tick_h));
        if let Some(cell) = &tick.cell {
            let (tw, _) = texts.size(widget, Role::MonoXs, cell);
            let current = view.frame_at_x(tick.x + 0.5) == playhead;
            let ink = if current { &p.text } else { &p.muted };
            texts.draw(
                s,
                widget,
                Role::MonoXs,
                cell,
                (tick.x + (view.px_per_frame - tw) / 2.0, 12.0),
                ink,
            );
        }
        if let Some(label) = &tick.label {
            let (y, ink) = match tick.kind {
                TickKind::Second => (1.0, &p.text),
                _ => (4.0, &p.muted),
            };
            texts.draw(s, widget, Role::MonoXs, label, (tick.x + 4.0, y), ink);
        }
    }
    // The ruler's bottom hairline, over the ticks' last row (S02: `border`
    // at y 719 across the time area; WP10's shot review).
    s.append_color(&p.border, &rect(0.0, height - 1.0, w, 1.0));
    s.pop();
}

// ------------------------------------------------------------- the overlay

/// What the overlay shows.
pub struct Overlay {
    /// The playhead's x in the time area.
    pub playhead_x: f64,
    /// The scrub flag's timecode while scrubbing.
    pub flag: Option<String>,
    /// The snap guide's x while it flashes.
    pub flash: Option<f64>,
    /// Where the rows end (the playhead's line stops at the last lane),
    /// from the overlay's top (the ruler's).
    pub bottom: f64,
    pub ruler: f64,
}

/// The playhead (C18): a 13×12 head at the top of the ruler and a 2 px
/// line to the last lane; the scrub flag; the snap guide.
pub fn overlay(
    s: &gtk::Snapshot,
    widget: &gtk::Widget,
    texts: &Texts,
    p: &Palette,
    view: &Viewport,
    o: &Overlay,
) {
    s.push_clip(&rect(0.0, 0.0, view.width, o.bottom.max(o.ruler)));
    if let Some(x) = o.flash {
        s.append_color(&p.snap_guide, &rect(x, o.ruler, 1.0, o.bottom - o.ruler));
    }
    let x = o.playhead_x;
    if x >= -8.0 && x <= view.width + 8.0 {
        let head = x - 6.5;
        s.append_color(&p.playhead, &rect(head + 5.5, 0.0, 2.0, o.bottom));
        let builder = gsk::PathBuilder::new();
        let (hx, hy) = (head as f32, 0.0f32);
        builder.move_to(hx, hy);
        builder.line_to(hx + 13.0, hy);
        builder.line_to(hx + 13.0, hy + 6.0);
        builder.line_to(hx + 6.5, hy + 12.0);
        builder.line_to(hx, hy + 6.0);
        builder.close();
        s.append_fill(&builder.to_path(), gsk::FillRule::Winding, &p.playhead);
        if let Some(flag) = &o.flag {
            let (tw, _) = texts.size(widget, Role::MonoSm, flag);
            let r = rect(x + 10.0, 3.0, tw + 10.0, 20.0);
            paint::rounded(s, &r, 4.0, &p.raised);
            paint::inner_stroke(s, &r, 1.0, &p.playhead);
            texts.draw(s, widget, Role::MonoSm, flag, (x + 15.0, 5.0), &p.text);
        }
    }
    s.pop();
}
