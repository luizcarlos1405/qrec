//! Scene commands: layers, audio tracks, scene apply (A§4.4 scene rules,
//! A§2.1 defaults). Owner: scene.
//!
//! The project scene is what new takes record; each take keeps its own
//! snapshot, and a take's media is a fact. So layers and tracks are added
//! only to the project scene, a take's sources never change, removing from
//! a take touches only its snapshot (the stream stays), and the `missing`
//! flags (facts) are never edited or copied. New ids come from
//! `ctx::next_layer_id` / `ctx::next_track_id` (never reused); labels come
//! from `ctx.facts` (screens, cameras, audio devices).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::apply::Outcome;
use crate::apply::project::{clean_text, require_project};
use crate::ctx::{self, AudioDeviceInfo, Ctx, Facts};
use crate::error::CoreError;
use crate::history::commit;
use crate::ids::{LayerId, TakeId, TrackId};
use crate::model::{
    AudioRole, AudioTrack, Canvas, Crop, Fit, Layer, Project, Rect, Scene, Stream, StreamKind,
    TakeStatus, VideoSource,
};
use crate::protocol::command::{
    AudioPatch, Command, LayerPatch, SceneDest, SceneTarget, VideoSourceArg,
};
use crate::protocol::reply::CmdResult;
use crate::state::State;

/// `gainDb` range (A§2.3).
const MAX_GAIN_DB: f64 = 60.0;
/// `|syncOffsetMs|` limit (A§2.3).
const MAX_SYNC_OFFSET_MS: i32 = 5000;

// ------------------------------------------------------------------- layers

/// `qrec scene layer add screen|camera`. Only `SceneTarget::Project`;
/// unknown output/device ⇒ `NotFound`; a source already in the scene ⇒
/// `Invalid`. Defaults (A§2.1): a screen is named `Screen <output>` and
/// fills the canvas (`contain`); a camera is named `Camera`, 20 % of the
/// canvas width at 4:3, bottom-right with a 2 % margin (`cover`). The new
/// layer goes on top.
pub fn layer_add(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    source: VideoSourceArg,
    name: Option<String>,
    rect: Option<Rect>,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::LayerAdd {
        target: target.clone(),
        source: source.clone(),
        name: name.clone(),
        rect,
    }
    .to_json();
    project_only(&target, "layers")?;
    let name = name.map(|n| clean_text("layer name", &n)).transpose()?;
    if let Some(rect) = rect {
        check_rect(rect)?;
    }
    let project = &state.project;
    let canvas = &project.canvas;
    let facts = &ctx.facts;
    let (source, default_name, default_rect, fit) = match source {
        VideoSourceArg::Screen { output } => {
            let Some(screen) = facts.screens.iter().find(|s| s.output == output) else {
                let known: Vec<&str> = facts.screens.iter().map(|s| s.output.as_str()).collect();
                return Err(CoreError::not_found(format!("no screen named {output:?}"))
                    .with_hint(known_sources("screens", &known)));
            };
            let label = non_empty(&screen.description).unwrap_or(&output).to_owned();
            let name = format!("Screen {output}");
            let rect = Rect::new(0, 0, canvas.width, canvas.height);
            (
                VideoSource::Screen { output, label },
                name,
                rect,
                Fit::Contain,
            )
        }
        VideoSourceArg::Camera { device } => {
            let Some(camera) = facts.cameras.iter().find(|c| c.device == device) else {
                let known: Vec<&str> = facts.cameras.iter().map(|c| c.device.as_str()).collect();
                return Err(CoreError::not_found(format!("no camera at {device:?}"))
                    .with_hint(known_sources("cameras", &known)));
            };
            let label = non_empty(&camera.label).unwrap_or(&device).to_owned();
            let source = VideoSource::Camera { device, label };
            (source, "Camera".to_owned(), camera_rect(canvas), Fit::Cover)
        }
    };
    if let Some(existing) = project
        .scene
        .layers
        .iter()
        .find(|l| same_video_source(&l.source, &source))
    {
        return Err(CoreError::invalid(format!(
            "{} {} is already layer {}",
            source.kind_str(),
            video_source_name(&source),
            existing.id
        ))
        .with_hint(format!(
            "change that layer instead (`qrec scene layer set {}`)",
            existing.id
        )));
    }

    let layer = Layer {
        id: ctx::next_layer_id(state),
        name: name.unwrap_or(default_name),
        visible: true,
        opacity: 1.0,
        source,
        rect: rect.unwrap_or(default_rect),
        crop: Crop::default(),
        fit,
        locked: false,
        missing: false,
    };
    let mut next = state.clone();
    next.project.scene.layers.push(layer.clone());
    let what = format!("layer {} ({})", layer.id, layer.name);
    let result = CmdResult::new(format!("Added {what} on top of the project scene"))
        .with_created([&layer.id])
        .with_result(&layer);
    let committed = commit(state, ctx, format!("Add {what}"), cmd, next);
    Ok(Outcome::done(committed, result))
}

/// A camera's default rect (A§2.1): 20 % of the canvas width at 4:3 (even
/// sizes), bottom-right with a margin of 2 % of the canvas width.
fn camera_rect(canvas: &Canvas) -> Rect {
    let w = ((canvas.width / 5) & !1).max(2);
    let h = ((w * 3 / 4) & !1).max(2);
    let margin = canvas.width / 50;
    Rect::new(
        canvas.width.saturating_sub(w + margin) as i32,
        canvas.height.saturating_sub(h + margin) as i32,
        w,
        h,
    )
}

fn same_video_source(a: &VideoSource, b: &VideoSource) -> bool {
    match (a, b) {
        (VideoSource::Screen { output: x, .. }, VideoSource::Screen { output: y, .. }) => x == y,
        (VideoSource::Camera { device: x, .. }, VideoSource::Camera { device: y, .. }) => x == y,
        _ => false,
    }
}

fn video_source_name(source: &VideoSource) -> &str {
    match source {
        VideoSource::Screen { output, .. } => output,
        VideoSource::Camera { device, .. } => device,
    }
}

/// `qrec scene layer set <id> [--take T] …`: on the project scene or one
/// take's snapshot. `missing` and the source can't be set.
pub fn layer_set(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
    patch: LayerPatch,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::LayerSet {
        target: target.clone(),
        layer: layer.clone(),
        patch: patch.clone(),
    }
    .to_json();
    let LayerPatch {
        name,
        visible,
        opacity,
        rect,
        crop,
        fit,
        locked,
    } = patch;
    if name.is_none()
        && visible.is_none()
        && opacity.is_none()
        && rect.is_none()
        && crop.is_none()
        && fit.is_none()
        && locked.is_none()
    {
        return Err(CoreError::usage("nothing to change").with_hint(
            "qrec scene layer set <id> [--rect X,Y,W,H] [--crop T,R,B,L] [--opacity 0-1] \
             [--visible BOOL] [--fit contain|cover] [--name N] [--locked BOOL] [--take TAKE]",
        ));
    }
    let name = name.map(|n| clean_text("layer name", &n)).transpose()?;
    if let Some(opacity) = opacity {
        check_opacity(opacity)?;
    }
    if let Some(rect) = rect {
        check_rect(rect)?;
    }

    let mut next = state.clone();
    let scene = scene_mut(&mut next.project, &target)?;
    let index = layer_index(scene, &target, &layer)?;
    let l = &mut scene.layers[index];
    let mut changes = Vec::new();
    if let Some(v) = name.filter(|v| *v != l.name) {
        changes.push(format!("name {v:?}"));
        l.name = v;
    }
    if let Some(v) = visible.filter(|v| *v != l.visible) {
        changes.push(if v { "shown" } else { "hidden" }.to_owned());
        l.visible = v;
    }
    if let Some(v) = opacity.filter(|v| *v != l.opacity) {
        changes.push(format!("opacity {v}"));
        l.opacity = v;
    }
    if let Some(v) = rect.filter(|v| *v != l.rect) {
        changes.push(format!("rect {},{},{},{}", v.x, v.y, v.w, v.h));
        l.rect = v;
    }
    if let Some(v) = crop.filter(|v| *v != l.crop) {
        changes.push(format!(
            "crop {},{},{},{}",
            v.top, v.right, v.bottom, v.left
        ));
        l.crop = v;
    }
    if let Some(v) = fit.filter(|v| *v != l.fit) {
        changes.push(format!("fit {}", fit_label(v)));
        l.fit = v;
    }
    if let Some(v) = locked.filter(|v| *v != l.locked) {
        changes.push(if v { "locked" } else { "unlocked" }.to_owned());
        l.locked = v;
    }
    let what = format!("layer {layer}{}", in_scene(&target));
    let (label, message) = if changes.is_empty() {
        (
            String::new(),
            format!("Nothing changed: {what} already has these settings"),
        )
    } else {
        let changes = changes.join(", ");
        (
            format!("Change {what}: {changes}"),
            format!("Changed {what}: {changes}"),
        )
    };
    let view = scene_ref(&next.project, &target)
        .ok()
        .and_then(|s| s.layer(&layer))
        .map(|l| serde_json::to_value(l).expect("layers serialize"))
        .unwrap_or_default();
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_result(&view),
    ))
}

/// `qrec scene layer order <id> --to N`: moves the layer to z-index `to`
/// (0 = bottom); an index past the top means the top.
pub fn layer_order(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
    to: usize,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::LayerOrder {
        target: target.clone(),
        layer: layer.clone(),
        to,
    }
    .to_json();
    let mut next = state.clone();
    let scene = scene_mut(&mut next.project, &target)?;
    let from = layer_index(scene, &target, &layer)?;
    let top = scene.layers.len() - 1;
    let to = to.min(top);
    let moved = scene.layers.remove(from);
    scene.layers.insert(to, moved);
    let order: Vec<String> = scene.layers.iter().map(|l| l.id.to_string()).collect();
    let place = match to {
        _ if top == 0 => format!("index {to}"),
        0 => "index 0 (bottom)".to_owned(),
        _ if to == top => format!("index {to} (top)"),
        _ => format!("index {to}"),
    };
    let what = format!("layer {layer}{}", in_scene(&target));
    let (label, message) = if from == to {
        (
            String::new(),
            format!("Nothing changed: {what} is already at {place}"),
        )
    } else {
        (
            format!("Move {what} to {place}"),
            format!("Moved {what} to {place}"),
        )
    };
    let committed = commit(state, ctx, label, cmd, next);
    let result = CmdResult::new(message).with_result(&json!({ "order": order }));
    Ok(Outcome::done(committed, result))
}

/// `qrec scene layer remove <id> [--take T]`. On a take this removes the
/// layer from that snapshot only; the stream (and file) stays, and a later
/// `SceneApply` from the project brings it back.
pub fn layer_remove(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::LayerRemove {
        target: target.clone(),
        layer: layer.clone(),
    }
    .to_json();
    let mut next = state.clone();
    let scene = scene_mut(&mut next.project, &target)?;
    let index = layer_index(scene, &target, &layer)?;
    let removed = scene.layers.remove(index);
    let what = format!("layer {} ({})", removed.id, removed.name);
    let (label, message) = match &target {
        SceneTarget::Project => (
            format!("Remove {what}"),
            format!("Removed {what} from the project scene; new takes won't record it"),
        ),
        SceneTarget::Take(take) => (
            format!("Remove {what} from take {}", take.number()),
            format!(
                "Removed {what} from take {}'s scene; its file stays \
                 (`qrec scene apply --take {take}` brings it back)",
                take.number()
            ),
        ),
    };
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_removed([&removed.id]),
    ))
}

// ------------------------------------------------------------------- audio

/// `qrec scene audio add mic|system [--source NAME] [--label L]`. Only
/// `SceneTarget::Project`. The source defaults to `facts.default_mic` /
/// `default_monitor`; the label to the device description, else `Mic` /
/// `System audio` (A§2.1). A monitor can't be a mic (SPEC §5.1), and a
/// source already in the scene is `Invalid`.
pub fn audio_add(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    role: AudioRole,
    source: Option<String>,
    label: Option<String>,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::AudioAdd {
        target: target.clone(),
        role,
        source: source.clone(),
        label: label.clone(),
    }
    .to_json();
    project_only(&target, "audio tracks")?;
    let label = label.map(|l| clean_text("track label", &l)).transpose()?;
    let facts = &ctx.facts;
    let (source, device) = match source {
        Some(name) => {
            let name = clean_text("audio source", &name)?;
            let device = find_device(facts, &name);
            (name, device)
        }
        None => {
            let default = match role {
                AudioRole::Mic => &facts.default_mic,
                AudioRole::System => &facts.default_monitor,
            };
            let device = default.clone().ok_or_else(|| no_default_device(role))?;
            (device.name.clone(), Some(device))
        }
    };
    check_role(role, &source, device.as_ref())?;
    if let Some(existing) = state
        .project
        .scene
        .audio
        .iter()
        .find(|t| t.source == source)
    {
        return Err(
            CoreError::invalid(format!("{source} is already track {}", existing.id)).with_hint(
                format!(
                    "change that track instead (`qrec scene audio set {}`)",
                    existing.id
                ),
            ),
        );
    }
    let label = label
        .or_else(|| {
            device
                .as_ref()
                .and_then(|d| non_empty(&d.description))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| default_track_label(role).to_owned());

    let track = AudioTrack {
        id: ctx::next_track_id(state),
        role,
        source,
        label,
        gain_db: 0.0,
        muted: false,
        sync_offset_ms: 0,
        missing: false,
    };
    let mut next = state.clone();
    next.project.scene.audio.push(track.clone());
    let what = format!("audio track {} ({})", track.id, track.label);
    let kind = match role {
        AudioRole::Mic => "mic",
        AudioRole::System => "system audio",
    };
    let result = CmdResult::new(format!(
        "Added {kind} track {} ({}) from {}",
        track.id, track.label, track.source
    ))
    .with_created([&track.id])
    .with_result(&track);
    let committed = commit(state, ctx, format!("Add {what}"), cmd, next);
    Ok(Outcome::done(committed, result))
}

/// `qrec scene audio set <id> [--take T] …`. The source can change only in
/// the project scene (a take recorded what it recorded); a new source takes
/// the device description as its label unless one is given.
pub fn audio_set(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    track: TrackId,
    patch: AudioPatch,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::AudioSet {
        target: target.clone(),
        track: track.clone(),
        patch: patch.clone(),
    }
    .to_json();
    let AudioPatch {
        label,
        source,
        gain_db,
        muted,
        sync_offset_ms,
    } = patch;
    if label.is_none()
        && source.is_none()
        && gain_db.is_none()
        && muted.is_none()
        && sync_offset_ms.is_none()
    {
        return Err(CoreError::usage("nothing to change").with_hint(
            "qrec scene audio set <id> [--gain-db DB] [--mute BOOL] [--sync-offset-ms MS] \
             [--label L] [--source NAME] [--take TAKE]",
        ));
    }
    let label = label.map(|l| clean_text("track label", &l)).transpose()?;
    let source = source.map(|s| clean_text("audio source", &s)).transpose()?;
    if let Some(gain) = gain_db {
        check_gain(gain)?;
    }
    if let Some(offset) = sync_offset_ms {
        check_sync_offset(offset)?;
    }

    let mut next = state.clone();
    let scene = scene_mut(&mut next.project, &target)?;
    let index = track_index(scene, &target, &track)?;
    let device = source.as_deref().and_then(|s| find_device(&ctx.facts, s));
    let current = &scene.audio[index];
    if let Some(source) = source.as_ref().filter(|s| **s != current.source) {
        if let SceneTarget::Take(take) = &target {
            return Err(CoreError::invalid(format!(
                "take {} recorded {track} from {}; a take's sources are facts",
                take.number(),
                current.source
            ))
            .with_hint(
                "change the source in the project scene (without --take); new takes record it",
            ));
        }
        check_role(current.role, source, device.as_ref())?;
        if let Some(other) = scene
            .audio
            .iter()
            .find(|t| t.id != track && t.source == *source)
        {
            return Err(CoreError::invalid(format!(
                "{source} is already track {}",
                other.id
            )));
        }
    }

    let t = &mut scene.audio[index];
    let mut changes = Vec::new();
    if let Some(v) = source.filter(|v| *v != t.source) {
        changes.push(format!("source {v}"));
        t.source = v;
        let described = device.as_ref().and_then(|d| non_empty(&d.description));
        if let Some(desc) = described.filter(|d| label.is_none() && *d != t.label) {
            changes.push(format!("label {desc:?}"));
            t.label = desc.to_owned();
        }
    }
    if let Some(v) = label.filter(|v| *v != t.label) {
        changes.push(format!("label {v:?}"));
        t.label = v;
    }
    if let Some(v) = gain_db.filter(|v| *v != t.gain_db) {
        changes.push(format!("gain {v} dB"));
        t.gain_db = v;
    }
    if let Some(v) = muted.filter(|v| *v != t.muted) {
        changes.push(if v { "muted" } else { "unmuted" }.to_owned());
        t.muted = v;
    }
    if let Some(v) = sync_offset_ms.filter(|v| *v != t.sync_offset_ms) {
        changes.push(format!("sync offset {v} ms"));
        t.sync_offset_ms = v;
    }
    let view = serde_json::to_value(&*t).expect("tracks serialize");
    let what = format!("audio track {track}{}", in_scene(&target));
    let (label, message) = if changes.is_empty() {
        (
            String::new(),
            format!("Nothing changed: {what} already has these settings"),
        )
    } else {
        let changes = changes.join(", ");
        (
            format!("Change {what}: {changes}"),
            format!("Changed {what}: {changes}"),
        )
    };
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_result(&view),
    ))
}

/// `qrec scene audio remove <id> [--take T]`. Removing the project track
/// that `autocut.track` names also sets it to null, in the same entry (a
/// take's snapshot doesn't change what the other takes use).
pub fn audio_remove(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    track: TrackId,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::AudioRemove {
        target: target.clone(),
        track: track.clone(),
    }
    .to_json();
    let mut next = state.clone();
    let scene = scene_mut(&mut next.project, &target)?;
    let index = track_index(scene, &target, &track)?;
    let removed = scene.audio.remove(index);
    let what = format!("audio track {} ({})", removed.id, removed.label);
    let (mut label, mut message) = match &target {
        SceneTarget::Project => (
            format!("Remove {what}"),
            format!("Removed {what} from the project scene; new takes won't record it"),
        ),
        SceneTarget::Take(take) => (
            format!("Remove {what} from take {}", take.number()),
            format!(
                "Removed {what} from take {}'s scene; its file stays \
                 (`qrec scene apply --take {take}` brings it back)",
                take.number()
            ),
        ),
    };
    if target == SceneTarget::Project && next.project.autocut.track.as_ref() == Some(&track) {
        next.project.autocut.track = None;
        label.push_str(" (autocut track reset)");
        message.push_str("; autocut now uses each take's first mic track");
    }
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_removed([&removed.id]),
    ))
}

fn find_device(facts: &Facts, name: &str) -> Option<AudioDeviceInfo> {
    facts
        .audio_devices
        .iter()
        .chain(facts.default_mic.iter())
        .chain(facts.default_monitor.iter())
        .find(|d| d.name == name)
        .cloned()
}

fn no_default_device(role: AudioRole) -> CoreError {
    let what = match role {
        AudioRole::Mic => "no default microphone was found",
        AudioRole::System => "no default output monitor (system audio) was found",
    };
    CoreError::not_found(what).with_hint("pass --source NAME (see `qrec sources list`)")
}

/// A sink's monitor can't be a mic: that was qrec-tui's data bug (SPEC §5.1).
fn check_role(
    role: AudioRole,
    source: &str,
    device: Option<&AudioDeviceInfo>,
) -> Result<(), CoreError> {
    let monitor = device.is_some_and(|d| d.is_monitor) || source.ends_with(".monitor");
    if role == AudioRole::Mic && monitor {
        return Err(
            CoreError::invalid(format!("{source} is an output monitor, not a microphone"))
                .with_hint(format!(
                    "record it as system audio: `qrec scene audio add system --source {source}`"
                )),
        );
    }
    Ok(())
}

fn default_track_label(role: AudioRole) -> &'static str {
    match role {
        AudioRole::Mic => "Mic",
        AudioRole::System => "System audio",
    }
}

// -------------------------------------------------------------- scene apply

/// `qrec scene apply`. The four CLI forms: `--take T` (project → T, "reset
/// take to project scene"), `--to-all-takes` (project → every take),
/// `--from-take T` (T → project, "use this layout for new takes") and
/// `--from-take T --to-all-takes` (T → every other take).
///
/// Matches layers and tracks by id and copies layout fields only (A§4.4):
/// `rect, crop, opacity, visible, fit` and the z-order of matched layers;
/// `gainDb, muted, syncOffsetMs` of tracks. Names, labels, locks, sources
/// and `missing` are never copied. A take gets back the layers and tracks
/// of the source scene it has a stream for but whose entry its snapshot
/// lost; nothing else is ever added, and nothing is removed. `AllTakes`
/// skips takes that are still recording.
pub fn scene_apply(
    state: &State,
    ctx: &Ctx,
    from: SceneTarget,
    to: SceneDest,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneApply {
        from: from.clone(),
        to: to.clone(),
    }
    .to_json();
    let project = &state.project;
    let source = scene_ref(project, &from)?.clone();
    let from_take = match &from {
        SceneTarget::Project => None,
        SceneTarget::Take(take) => Some(take),
    };
    let dests: Vec<SceneTarget> = match &to {
        SceneDest::Project => {
            if from_take.is_none() {
                return Err(
                    CoreError::usage("the project scene can't be applied to itself").with_hint(
                        "pass --take TAKE or --to-all-takes to copy it into takes, \
                         or --from-take TAKE to copy a take's layout into it",
                    ),
                );
            }
            vec![SceneTarget::Project]
        }
        SceneDest::Takes(ids) => {
            if ids.is_empty() {
                return Err(CoreError::usage("no takes to apply the layout to")
                    .with_hint("pass --take TAKE (or --to-all-takes)"));
            }
            let mut seen = BTreeSet::new();
            let mut dests = Vec::new();
            for id in ids {
                let take = project.take(id).ok_or_else(|| no_take(id))?;
                editable(id, take.status)?;
                if Some(id) != from_take && seen.insert(id) {
                    dests.push(SceneTarget::Take(id.clone()));
                }
            }
            if dests.is_empty() {
                return Err(CoreError::usage(format!(
                    "take {} can't be applied to itself",
                    ids[0].number()
                )));
            }
            dests
        }
        SceneDest::AllTakes => project
            .takes
            .iter()
            .filter(|(id, take)| take.status != TakeStatus::Recording && Some(*id) != from_take)
            .map(|(id, _)| SceneTarget::Take(id.clone()))
            .collect(),
    };

    let mut next = state.clone();
    let mut added: Vec<(&SceneTarget, Vec<String>)> = Vec::new();
    for dest in &dests {
        let ids = match dest {
            SceneTarget::Project => copy_layout(&source, &mut next.project.scene, None),
            SceneTarget::Take(id) => match next.project.takes.get_mut(id) {
                Some(take) => copy_layout(&source, &mut take.scene, Some(take.streams.as_slice())),
                None => Vec::new(),
            },
        };
        if !ids.is_empty() {
            added.push((dest, ids));
        }
    }

    let from_name = match &from {
        SceneTarget::Project => "the project layout".to_owned(),
        SceneTarget::Take(take) => format!("take {}'s layout", take.number()),
    };
    let to_name = match &to {
        SceneDest::Project => "the project scene".to_owned(),
        SceneDest::AllTakes => "all takes".to_owned(),
        SceneDest::Takes(_) => takes_phrase(&dests),
    };
    let label = format!("Apply {from_name} to {to_name}");
    let message = if dests.is_empty() {
        let other = if from_take.is_some() { "other " } else { "" };
        format!("Nothing changed: there are no {other}takes to apply {from_name} to")
    } else if next.project == state.project {
        format!(
            "Nothing changed: {to_name} already match{} {from_name}",
            if dests.len() == 1 { "es" } else { "" }
        )
    } else {
        let mut message = format!("Applied {from_name} to {to_name}");
        if matches!(to, SceneDest::AllTakes) {
            message.push_str(&format!(
                " ({} take{})",
                dests.len(),
                if dests.len() == 1 { "" } else { "s" }
            ));
        }
        let readded: Vec<String> = added
            .iter()
            .map(|(dest, ids)| format!("{} to {}", ids.join(", "), scene_short(dest)))
            .collect();
        if !readded.is_empty() {
            message.push_str(&format!("; re-added {}", readded.join("; ")));
        }
        message
    };
    let scenes: Vec<String> = dests.iter().map(ToString::to_string).collect();
    let added: BTreeMap<String, &Vec<String>> = added
        .iter()
        .map(|(dest, ids)| (dest.to_string(), ids))
        .collect();
    let result = CmdResult::new(message).with_result(&json!({ "scenes": scenes, "added": added }));
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(committed, result))
}

/// Copies the layout of `from` into `dest` (see [`scene_apply`]). With
/// `streams` (`dest` is a take's scene) the layers and tracks of `from`
/// that the take has a stream for but `dest` lacks are added back; their
/// `missing` flag says whether that stream has media. Returns the ids added.
fn copy_layout(from: &Scene, dest: &mut Scene, streams: Option<&[Stream]>) -> Vec<String> {
    for layer in &mut dest.layers {
        if let Some(src) = from.layer(&layer.id) {
            layer.rect = src.rect;
            layer.crop = src.crop;
            layer.opacity = src.opacity;
            layer.visible = src.visible;
            layer.fit = src.fit;
        }
    }
    for track in &mut dest.audio {
        if let Some(src) = from.track(&track.id) {
            track.gain_db = src.gain_db;
            track.muted = src.muted;
            track.sync_offset_ms = src.sync_offset_ms;
        }
    }
    let Some(streams) = streams else {
        reorder_layers(from, dest, Vec::new());
        return Vec::new();
    };
    let stream_of =
        |kind: StreamKind, id: &str| streams.iter().find(|s| s.kind == kind && s.id == id);

    let mut seen = BTreeSet::new();
    let new_layers: Vec<Layer> = from
        .layers
        .iter()
        .filter(|l| dest.layer(&l.id).is_none() && seen.insert(&l.id))
        .filter_map(|l| {
            let stream = stream_of(StreamKind::Video, l.id.as_str())?;
            Some(Layer {
                missing: !has_media(stream),
                ..l.clone()
            })
        })
        .collect();
    let mut added: Vec<String> = new_layers.iter().map(|l| l.id.to_string()).collect();
    reorder_layers(from, dest, new_layers);

    for src in &from.audio {
        if dest.track(&src.id).is_some() {
            continue;
        }
        let Some(stream) = stream_of(StreamKind::Audio, src.id.as_str()) else {
            continue;
        };
        let at = track_insert_index(from, dest, &src.id);
        dest.audio.insert(
            at,
            AudioTrack {
                missing: !has_media(stream),
                ..src.clone()
            },
        );
        added.push(src.id.to_string());
    }
    added
}

/// Puts the layers `dest` shares with `from` (and the `added` ones) in
/// `from`'s z-order. They take the slots the shared layers had; the other
/// layers of `dest` keep theirs; added layers that find no slot go on top.
fn reorder_layers(from: &Scene, dest: &mut Scene, added: Vec<Layer>) {
    let mut pool: BTreeMap<LayerId, Layer> = BTreeMap::new();
    let mut slots: Vec<Option<Layer>> = Vec::with_capacity(dest.layers.len());
    for layer in std::mem::take(&mut dest.layers) {
        if from.layer(&layer.id).is_some() && !pool.contains_key(&layer.id) {
            pool.insert(layer.id.clone(), layer);
            slots.push(None);
        } else {
            slots.push(Some(layer));
        }
    }
    for layer in added {
        pool.insert(layer.id.clone(), layer);
    }
    let mut ordered = from.layers.iter().filter_map(|l| pool.remove(&l.id));
    let mut layers = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Some(layer) => layers.push(layer),
            None => layers.extend(ordered.next()),
        }
    }
    layers.extend(ordered);
    dest.layers = layers;
}

/// Where a track added back to `dest` goes: after the nearest track that
/// precedes it in `from` and is in `dest`, else first.
fn track_insert_index(from: &Scene, dest: &Scene, id: &TrackId) -> usize {
    let pos = from.audio.iter().position(|t| &t.id == id).unwrap_or(0);
    from.audio[..pos]
        .iter()
        .rev()
        .find_map(|t| dest.audio.iter().position(|d| d.id == t.id))
        .map_or(0, |i| i + 1)
}

/// The stream was probed (it has the media fields of its kind).
fn has_media(stream: &Stream) -> bool {
    match stream.kind {
        StreamKind::Video => stream.width.is_some() && stream.height.is_some(),
        StreamKind::Audio => stream.sample_rate.is_some(),
    }
}

/// "take 3" / "takes 1, 2".
fn takes_phrase(dests: &[SceneTarget]) -> String {
    let numbers: Vec<String> = dests
        .iter()
        .filter_map(|d| match d {
            SceneTarget::Take(take) => Some(take.number().to_string()),
            SceneTarget::Project => None,
        })
        .collect();
    match numbers.as_slice() {
        [one] => format!("take {one}"),
        many => format!("takes {}", many.join(", ")),
    }
}

// ------------------------------------------------------------------ helpers

/// `LayerAdd`/`AudioAdd` accept only the project scene: a take's media is a fact.
fn project_only(target: &SceneTarget, what: &str) -> Result<(), CoreError> {
    match target {
        SceneTarget::Project => Ok(()),
        SceneTarget::Take(take) => Err(CoreError::invalid(format!(
            "{what} can only be added to the project scene, not to {take}"
        ))
        .with_hint(
            "a take's media is a fact: add it to the project scene (without --take); new takes record it",
        )),
    }
}

/// The scene `target` names (a take must exist and be finished).
fn scene_ref<'a>(project: &'a Project, target: &SceneTarget) -> Result<&'a Scene, CoreError> {
    match target {
        SceneTarget::Project => Ok(&project.scene),
        SceneTarget::Take(id) => {
            let take = project.take(id).ok_or_else(|| no_take(id))?;
            editable(id, take.status)?;
            Ok(&take.scene)
        }
    }
}

fn scene_mut<'a>(
    project: &'a mut Project,
    target: &SceneTarget,
) -> Result<&'a mut Scene, CoreError> {
    match target {
        SceneTarget::Project => Ok(&mut project.scene),
        SceneTarget::Take(id) => {
            let take = project.takes.get_mut(id).ok_or_else(|| no_take(id))?;
            editable(id, take.status)?;
            Ok(&mut take.scene)
        }
    }
}

fn no_take(id: &TakeId) -> CoreError {
    CoreError::not_found(format!("no take {id}")).with_hint("see `qrec take list`")
}

/// A take still being recorded (or awaiting recovery) keeps its snapshot.
fn editable(id: &TakeId, status: TakeStatus) -> Result<(), CoreError> {
    if status == TakeStatus::Recording {
        return Err(
            CoreError::invalid(format!("take {} is still recording", id.number()))
                .with_hint("wait until it is finalized"),
        );
    }
    Ok(())
}

fn layer_index(scene: &Scene, target: &SceneTarget, id: &LayerId) -> Result<usize, CoreError> {
    scene.layer_index(id).ok_or_else(|| {
        let ids: Vec<&str> = scene.layers.iter().map(|l| l.id.as_str()).collect();
        CoreError::not_found(format!("no layer {id} in {}", scene_name(target))).with_hint(
            if ids.is_empty() {
                format!("{} has no layers", scene_name(target))
            } else {
                format!("layers there: {}", ids.join(", "))
            },
        )
    })
}

fn track_index(scene: &Scene, target: &SceneTarget, id: &TrackId) -> Result<usize, CoreError> {
    scene.audio.iter().position(|t| &t.id == id).ok_or_else(|| {
        let ids: Vec<&str> = scene.audio.iter().map(|t| t.id.as_str()).collect();
        CoreError::not_found(format!("no audio track {id} in {}", scene_name(target))).with_hint(
            if ids.is_empty() {
                format!("{} has no audio tracks", scene_name(target))
            } else {
                format!("tracks there: {}", ids.join(", "))
            },
        )
    })
}

/// "the project scene" / "take 3's scene".
fn scene_name(target: &SceneTarget) -> String {
    match target {
        SceneTarget::Project => "the project scene".to_owned(),
        SceneTarget::Take(take) => format!("take {}'s scene", take.number()),
    }
}

/// "the project scene" / "take 3".
fn scene_short(target: &SceneTarget) -> String {
    match target {
        SceneTarget::Project => "the project scene".to_owned(),
        SceneTarget::Take(take) => format!("take {}", take.number()),
    }
}

/// "" / " in take 3" (labels and messages).
fn in_scene(target: &SceneTarget) -> String {
    match target {
        SceneTarget::Project => String::new(),
        SceneTarget::Take(take) => format!(" in take {}", take.number()),
    }
}

fn known_sources(kind: &str, known: &[&str]) -> String {
    if known.is_empty() {
        format!("no {kind} were detected (see `qrec sources list` and `qrec doctor`)")
    } else {
        format!("{kind}: {} (see `qrec sources list`)", known.join(", "))
    }
}

fn non_empty(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|t| !t.is_empty())
}

fn check_rect(rect: Rect) -> Result<(), CoreError> {
    if rect.w == 0 || rect.h == 0 {
        return Err(CoreError::usage(format!(
            "a rect needs a width and height of at least 1 (got {}x{})",
            rect.w, rect.h
        ))
        .with_hint("rects are X,Y,W,H in canvas pixels, e.g. 0,0,1920,1080"));
    }
    Ok(())
}

fn check_opacity(opacity: f64) -> Result<(), CoreError> {
    if !(0.0..=1.0).contains(&opacity) {
        return Err(CoreError::usage(format!(
            "opacity must be from 0.0 to 1.0 (got {opacity})"
        )));
    }
    Ok(())
}

fn check_gain(gain_db: f64) -> Result<(), CoreError> {
    if !gain_db.is_finite() || gain_db.abs() > MAX_GAIN_DB {
        return Err(CoreError::usage(format!(
            "gain must be from -60 to 60 dB (got {gain_db})"
        )));
    }
    Ok(())
}

fn check_sync_offset(offset_ms: i32) -> Result<(), CoreError> {
    if offset_ms.unsigned_abs() > MAX_SYNC_OFFSET_MS.unsigned_abs() {
        return Err(CoreError::usage(format!(
            "the sync offset must be from -5000 to 5000 ms (got {offset_ms})"
        )));
    }
    Ok(())
}

fn fit_label(fit: Fit) -> &'static str {
    match fit {
        Fit::Contain => "contain",
        Fit::Cover => "cover",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{CameraInfo, ScreenInfo};
    use crate::error::ErrorCode;
    use crate::model::Fps;
    use crate::protocol::effect::Effect;
    use crate::testkit::{self, ProjectBuilder};

    fn facts() -> Facts {
        let device = |name: &str, description: &str, is_monitor| AudioDeviceInfo {
            name: name.into(),
            description: description.into(),
            is_monitor,
        };
        let screen = |output: &str, description: &str| ScreenInfo {
            output: output.into(),
            description: description.into(),
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            scale: 1.0,
        };
        let mic = device("alsa_input.usb-Yeti", "Yeti Nano", false);
        let monitor = device(
            "alsa_output.pci.analog-stereo.monitor",
            "Monitor of Built-in Audio",
            true,
        );
        Facts {
            screens: vec![screen("DP-4", "GIGA-BYTE GS27QA"), screen("HDMI-A-1", "")],
            cameras: vec![CameraInfo {
                device: "/dev/video0".into(),
                label: "Logitech C920".into(),
                modes: vec![],
            }],
            audio_devices: vec![
                mic.clone(),
                device("alsa_input.pci.analog-stereo", "Built-in Mic", false),
                monitor.clone(),
            ],
            default_mic: Some(mic),
            default_monitor: Some(monitor),
            ..Facts::default()
        }
    }

    fn ctx() -> Ctx {
        let mut ctx = testkit::ctx("2026-10-14T12:30:00Z", 3);
        ctx.facts = facts();
        ctx
    }

    fn reply(out: &Outcome) -> &CmdResult {
        out.reply.as_ref().unwrap().as_ref().unwrap()
    }

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    fn lid(id: &str) -> LayerId {
        id.parse().unwrap()
    }

    fn tid(id: &str) -> TrackId {
        id.parse().unwrap()
    }

    fn layer_ids(scene: &Scene) -> Vec<&str> {
        scene.layers.iter().map(|l| l.id.as_str()).collect()
    }

    fn track_ids(scene: &Scene) -> Vec<&str> {
        scene.audio.iter().map(|t| t.id.as_str()).collect()
    }

    fn screen(output: &str) -> VideoSourceArg {
        VideoSourceArg::Screen {
            output: output.into(),
        }
    }

    fn camera(device: &str) -> VideoSourceArg {
        VideoSourceArg::Camera {
            device: device.into(),
        }
    }

    /// L1 screen + L2 camera + A1 mic + A2 system, two takes, two clips.
    fn studio() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-4")
                .camera_layer("L2", "/dev/video0")
                .mic("A1", "alsa_input.usb-Yeti")
                .system("A2", "alsa_output.pci.analog-stereo.monitor")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .clip("c001", "take-0001", 0, 300)
                .clip("c002", "take-0002", 0, 200)
                .build(),
        )
    }

    fn empty() -> State {
        testkit::state(ProjectBuilder::new().build())
    }

    // ------------------------------------------------------------- layer add

    #[test]
    fn a_new_screen_fills_the_canvas() {
        let state = empty();
        let out = layer_add(
            &state,
            &ctx(),
            SceneTarget::Project,
            screen("DP-4"),
            None,
            None,
        )
        .unwrap();
        let layer = &out.state.project.scene.layers[0];
        assert_eq!(layer.id.as_str(), "L1");
        assert_eq!(layer.name, "Screen DP-4");
        assert_eq!(
            layer.source,
            VideoSource::Screen {
                output: "DP-4".into(),
                label: "GIGA-BYTE GS27QA".into()
            }
        );
        assert_eq!(layer.rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(layer.fit, Fit::Contain);
        assert!(layer.visible && !layer.locked && !layer.missing);
        assert_eq!(layer.opacity, 1.0);
        assert_eq!(layer.crop, Crop::default());

        let r = reply(&out);
        assert_eq!(r.seq, Some(1));
        assert_eq!(r.created, ["L1"]);
        assert_eq!(r.result["id"], "L1");
        // A GUI host in Live mode also gets the live pipeline update.
        assert!(matches!(
            out.effects[..],
            [Effect::History(_), Effect::WriteProject, Effect::SetLive(Some(_))]
        ));
        assert_eq!(
            out.state.history.entries[0].label,
            "Add layer L1 (Screen DP-4)"
        );
        assert_eq!(out.state.history.entries[0].cmd["cmd"], "layerAdd");

        // No description: the label is the output name.
        let out = layer_add(
            &out.state,
            &ctx(),
            SceneTarget::Project,
            screen("HDMI-A-1"),
            None,
            None,
        )
        .unwrap();
        let layer = &out.state.project.scene.layers[1];
        assert_eq!(layer.id.as_str(), "L2");
        assert_eq!(layer.source.label(), "HDMI-A-1");
    }

    #[test]
    fn a_new_camera_sits_bottom_right() {
        let state = testkit::state(ProjectBuilder::new().screen_layer("L1", "DP-4").build());
        let out = layer_add(
            &state,
            &ctx(),
            SceneTarget::Project,
            camera("/dev/video0"),
            None,
            None,
        )
        .unwrap();
        let layers = &out.state.project.scene.layers;
        assert_eq!(layer_ids(&out.state.project.scene), ["L1", "L2"], "on top");
        let cam = &layers[1];
        assert_eq!(cam.name, "Camera");
        assert_eq!(cam.source.label(), "Logitech C920");
        assert_eq!(cam.rect, Rect::new(1498, 754, 384, 288));
        assert_eq!(cam.fit, Fit::Cover);
        // The same rect the testkit builds (A§2.1).
        let built = ProjectBuilder::new()
            .camera_layer("L9", "/dev/video0")
            .build();
        assert_eq!(cam.rect, built.scene.layers[0].rect);
        // Other canvases.
        assert_eq!(
            camera_rect(&Canvas {
                width: 1280,
                height: 720,
                ..Canvas::default()
            }),
            Rect::new(999, 503, 256, 192)
        );
        assert_eq!(
            camera_rect(&Canvas {
                width: 16,
                height: 16,
                ..Canvas::default()
            }),
            Rect::new(14, 14, 2, 2)
        );
    }

    #[test]
    fn layer_add_takes_a_name_and_rect() {
        let rect = Rect::new(-10, 20, 640, 360);
        let out = layer_add(
            &empty(),
            &ctx(),
            SceneTarget::Project,
            camera("/dev/video0"),
            Some(" Face ".into()),
            Some(rect),
        )
        .unwrap();
        let layer = &out.state.project.scene.layers[0];
        assert_eq!((layer.name.as_str(), layer.rect), ("Face", rect));
        assert_eq!(out.state.history.entries[0].label, "Add layer L1 (Face)");
    }

    #[test]
    fn layer_ids_are_never_reused() {
        // L3 lives only in take 1's snapshot; the next layer is L4.
        let mut state = studio();
        state.project.scene.layers.retain(|l| l.id.as_str() != "L2");
        let take1 = state.project.takes.get_mut(&take(1)).unwrap();
        let mut extra = take1.scene.layers[1].clone();
        extra.id = lid("L3");
        take1.scene.layers.push(extra);
        let out = layer_add(
            &state,
            &ctx(),
            SceneTarget::Project,
            camera("/dev/video0"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(reply(&out).created, ["L4"]);
    }

    #[test]
    fn layer_add_errors() {
        let state = studio();
        let add = |target, source, rect: Option<Rect>, name: Option<&str>| {
            layer_add(
                &state,
                &ctx(),
                target,
                source,
                name.map(str::to_owned),
                rect,
            )
            .unwrap_err()
        };
        let err = add(SceneTarget::Take(take(1)), screen("HDMI-A-1"), None, None);
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "layers can only be added to the project scene, not to take-0001"
        );
        let err = add(SceneTarget::Project, screen("DP-9"), None, None);
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no screen named \"DP-9\"");
        assert_eq!(
            err.hint.as_deref(),
            Some("screens: DP-4, HDMI-A-1 (see `qrec sources list`)")
        );
        let err = add(SceneTarget::Project, camera("/dev/video7"), None, None);
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = add(SceneTarget::Project, screen("DP-4"), None, None);
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "screen DP-4 is already layer L1");
        let err = add(SceneTarget::Project, camera("/dev/video0"), None, None);
        assert_eq!(err.message, "camera /dev/video0 is already layer L2");
        let err = add(
            SceneTarget::Project,
            screen("HDMI-A-1"),
            Some(Rect::new(0, 0, 0, 10)),
            None,
        );
        assert_eq!(err.code, ErrorCode::Usage);
        let err = add(SceneTarget::Project, screen("HDMI-A-1"), None, Some(""));
        assert_eq!(err.code, ErrorCode::Usage);

        let mut bare = ctx();
        bare.facts.screens.clear();
        let err = layer_add(
            &state,
            &bare,
            SceneTarget::Project,
            screen("DP-4"),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            err.hint.as_deref(),
            Some("no screens were detected (see `qrec sources list` and `qrec doctor`)")
        );
    }

    // ------------------------------------------------------------- layer set

    #[test]
    fn layer_set_edits_the_project_scene_or_one_take() {
        let state = studio();
        let patch = LayerPatch {
            opacity: Some(0.5),
            rect: Some(Rect::new(10, 20, 640, 360)),
            visible: Some(false),
            ..Default::default()
        };
        let out = layer_set(
            &state,
            &ctx(),
            SceneTarget::Project,
            lid("L2"),
            patch.clone(),
        )
        .unwrap();
        let l = out.state.project.scene.layer(&lid("L2")).unwrap();
        assert_eq!(
            (l.opacity, l.rect, l.visible),
            (0.5, Rect::new(10, 20, 640, 360), false)
        );
        assert_eq!(
            out.state.project.takes, state.project.takes,
            "takes keep their snapshots"
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Change layer L2: hidden, opacity 0.5, rect 10,20,640,360"
        );
        assert_eq!(reply(&out).result["opacity"], 0.5);

        let patch = LayerPatch {
            crop: Some(Crop {
                top: 0,
                right: 0,
                bottom: 40,
                left: 0,
            }),
            fit: Some(Fit::Contain),
            name: Some("Face".into()),
            locked: Some(true),
            ..Default::default()
        };
        let out = layer_set(&state, &ctx(), SceneTarget::Take(take(1)), lid("L2"), patch).unwrap();
        let t1 = &out.state.project.takes[&take(1)];
        let l = t1.scene.layer(&lid("L2")).unwrap();
        assert_eq!(l.crop.bottom, 40);
        assert_eq!(
            (l.fit, l.name.as_str(), l.locked),
            (Fit::Contain, "Face", true)
        );
        assert_eq!(out.state.project.scene, state.project.scene);
        assert_eq!(
            out.state.history.entries[0].label,
            "Change layer L2 in take 1: name \"Face\", crop 0,0,40,0, fit contain, locked"
        );
        assert_eq!(
            out.state.project.takes[&take(2)],
            state.project.takes[&take(2)]
        );

        // The same values again: nothing to append.
        let again = LayerPatch {
            opacity: Some(1.0),
            ..Default::default()
        };
        let out = layer_set(&state, &ctx(), SceneTarget::Project, lid("L1"), again).unwrap();
        assert_eq!(reply(&out).seq, None);
        assert!(out.effects.is_empty());
        assert_eq!(
            reply(&out).message,
            "Nothing changed: layer L1 already has these settings"
        );
    }

    #[test]
    fn layer_set_errors() {
        let state = studio();
        let set = |target, layer: &str, patch| {
            layer_set(&state, &ctx(), target, lid(layer), patch).unwrap_err()
        };
        let opacity = |v| LayerPatch {
            opacity: Some(v),
            ..Default::default()
        };
        assert_eq!(
            set(SceneTarget::Project, "L1", LayerPatch::default()).code,
            ErrorCode::Usage
        );
        assert_eq!(
            set(SceneTarget::Project, "L1", opacity(1.5)).code,
            ErrorCode::Usage
        );
        assert_eq!(
            set(SceneTarget::Project, "L1", opacity(f64::NAN)).code,
            ErrorCode::Usage
        );
        let rect = LayerPatch {
            rect: Some(Rect::new(0, 0, 10, 0)),
            ..Default::default()
        };
        assert_eq!(set(SceneTarget::Project, "L1", rect).code, ErrorCode::Usage);
        let err = set(SceneTarget::Project, "L7", opacity(0.5));
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no layer L7 in the project scene");
        assert_eq!(err.hint.as_deref(), Some("layers there: L1, L2"));
        let err = set(SceneTarget::Take(take(9)), "L1", opacity(0.5));
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no take take-0009");

        // A take still recording (awaiting recovery) keeps its snapshot.
        let mut recording = studio();
        recording.project.takes.get_mut(&take(2)).unwrap().status = TakeStatus::Recording;
        let err = layer_set(
            &recording,
            &ctx(),
            SceneTarget::Take(take(2)),
            lid("L1"),
            opacity(0.5),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "take 2 is still recording");
    }

    // ----------------------------------------------------------- layer order

    #[test]
    fn layer_order_moves_by_z_index() {
        let state = testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-4")
                .screen_layer("L2", "HDMI-A-1")
                .camera_layer("L3", "/dev/video0")
                .take("take-0001", 100)
                .build(),
        );
        let out = layer_order(&state, &ctx(), SceneTarget::Project, lid("L1"), 2).unwrap();
        assert_eq!(layer_ids(&out.state.project.scene), ["L2", "L3", "L1"]);
        assert_eq!(reply(&out).message, "Moved layer L1 to index 2 (top)");
        assert_eq!(reply(&out).result["order"], json!(["L2", "L3", "L1"]));

        // Past the top means the top.
        let out = layer_order(&state, &ctx(), SceneTarget::Project, lid("L2"), 99).unwrap();
        assert_eq!(layer_ids(&out.state.project.scene), ["L1", "L3", "L2"]);
        let out = layer_order(&state, &ctx(), SceneTarget::Project, lid("L3"), 0).unwrap();
        assert_eq!(layer_ids(&out.state.project.scene), ["L3", "L1", "L2"]);
        assert_eq!(
            out.state.history.entries[0].label,
            "Move layer L3 to index 0 (bottom)"
        );

        // In a take's snapshot only.
        let out = layer_order(&state, &ctx(), SceneTarget::Take(take(1)), lid("L3"), 1).unwrap();
        assert_eq!(
            layer_ids(&out.state.project.takes[&take(1)].scene),
            ["L1", "L3", "L2"]
        );
        assert_eq!(layer_ids(&out.state.project.scene), ["L1", "L2", "L3"]);
        assert_eq!(
            out.state.history.entries[0].label,
            "Move layer L3 in take 1 to index 1"
        );

        // Already there.
        let out = layer_order(&state, &ctx(), SceneTarget::Project, lid("L3"), 7).unwrap();
        assert_eq!(reply(&out).seq, None);
        assert_eq!(
            reply(&out).message,
            "Nothing changed: layer L3 is already at index 2 (top)"
        );
        let err = layer_order(&state, &ctx(), SceneTarget::Project, lid("L9"), 0).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    // ---------------------------------------------------------- layer remove

    #[test]
    fn layer_remove_from_the_project_or_a_take() {
        let state = studio();
        let out = layer_remove(&state, &ctx(), SceneTarget::Project, lid("L2")).unwrap();
        assert_eq!(layer_ids(&out.state.project.scene), ["L1"]);
        assert_eq!(out.state.project.takes, state.project.takes);
        assert_eq!(reply(&out).removed, ["L2"]);
        assert_eq!(
            out.state.history.entries[0].label,
            "Remove layer L2 (Camera)"
        );

        let out = layer_remove(&state, &ctx(), SceneTarget::Take(take(1)), lid("L2")).unwrap();
        let t1 = &out.state.project.takes[&take(1)];
        assert_eq!(layer_ids(&t1.scene), ["L1"]);
        assert!(t1.stream("L2").is_some(), "the stream stays");
        assert_eq!(out.state.project.scene, state.project.scene);
        assert_eq!(
            reply(&out).message,
            "Removed layer L2 (Camera) from take 1's scene; its file stays \
             (`qrec scene apply --take take-0001` brings it back)"
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Remove layer L2 (Camera) from take 1"
        );
        let err =
            layer_remove(&out.state, &ctx(), SceneTarget::Take(take(1)), lid("L2")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no layer L2 in take 1's scene");
    }

    // ------------------------------------------------------------- audio add

    #[test]
    fn audio_add_uses_the_default_devices() {
        let state = empty();
        let out = audio_add(
            &state,
            &ctx(),
            SceneTarget::Project,
            AudioRole::Mic,
            None,
            None,
        )
        .unwrap();
        let mic = &out.state.project.scene.audio[0];
        assert_eq!(mic.id.as_str(), "A1");
        assert_eq!(mic.role, AudioRole::Mic);
        assert_eq!(mic.source, "alsa_input.usb-Yeti");
        assert_eq!(mic.label, "Yeti Nano");
        assert_eq!(
            (mic.gain_db, mic.muted, mic.sync_offset_ms, mic.missing),
            (0.0, false, 0, false)
        );
        assert_eq!(reply(&out).created, ["A1"]);
        assert_eq!(
            reply(&out).message,
            "Added mic track A1 (Yeti Nano) from alsa_input.usb-Yeti"
        );

        let out = audio_add(
            &out.state,
            &ctx(),
            SceneTarget::Project,
            AudioRole::System,
            None,
            None,
        )
        .unwrap();
        let system = &out.state.project.scene.audio[1];
        assert_eq!(system.id.as_str(), "A2");
        assert_eq!(system.source, "alsa_output.pci.analog-stereo.monitor");
        assert_eq!(system.label, "Monitor of Built-in Audio");
        assert_eq!(
            out.state.history.entries[1].label,
            "Add audio track A2 (Monitor of Built-in Audio)"
        );
    }

    #[test]
    fn audio_add_with_an_explicit_source() {
        let state = empty();
        let add = |source: &str, label: Option<&str>| {
            audio_add(
                &state,
                &ctx(),
                SceneTarget::Project,
                AudioRole::Mic,
                Some(source.into()),
                label.map(str::to_owned),
            )
            .unwrap()
            .state
            .project
            .scene
            .audio[0]
                .clone()
        };
        assert_eq!(
            add("alsa_input.pci.analog-stereo", None).label,
            "Built-in Mic"
        );
        let unknown = add("alsa_input.unplugged", None);
        assert_eq!(
            (unknown.source.as_str(), unknown.label.as_str()),
            ("alsa_input.unplugged", "Mic")
        );
        assert_eq!(add("alsa_input.unplugged", Some("Guest")).label, "Guest");

        let system = audio_add(
            &state,
            &ctx(),
            SceneTarget::Project,
            AudioRole::System,
            Some("alsa_output.usb.monitor".into()),
            None,
        )
        .unwrap();
        assert_eq!(system.state.project.scene.audio[0].label, "System audio");
    }

    #[test]
    fn audio_add_errors() {
        let state = studio();
        let add = |target, role, source: Option<&str>| {
            audio_add(
                &state,
                &ctx(),
                target,
                role,
                source.map(str::to_owned),
                None,
            )
            .unwrap_err()
        };
        let err = add(
            SceneTarget::Take(take(1)),
            AudioRole::Mic,
            Some("alsa_input.pci.analog-stereo"),
        );
        assert_eq!(err.code, ErrorCode::Invalid);
        let err = add(
            SceneTarget::Project,
            AudioRole::Mic,
            Some("alsa_output.pci.analog-stereo.monitor"),
        );
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "alsa_output.pci.analog-stereo.monitor is an output monitor, not a microphone"
        );
        let err = add(SceneTarget::Project, AudioRole::Mic, Some("bluez.monitor"));
        assert_eq!(err.code, ErrorCode::Invalid, "named like a monitor");
        let err = add(SceneTarget::Project, AudioRole::Mic, None);
        assert_eq!(err.message, "alsa_input.usb-Yeti is already track A1");
        let err = add(SceneTarget::Project, AudioRole::Mic, Some(" "));
        assert_eq!(err.code, ErrorCode::Usage);

        let mut bare = ctx();
        bare.facts = Facts::default();
        let err = audio_add(
            &empty(),
            &bare,
            SceneTarget::Project,
            AudioRole::Mic,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no default microphone was found");
        let err = audio_add(
            &empty(),
            &bare,
            SceneTarget::Project,
            AudioRole::System,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "no default output monitor (system audio) was found"
        );
    }

    // ------------------------------------------------------------- audio set

    #[test]
    fn audio_set_edits_mix_settings() {
        let state = studio();
        let patch = AudioPatch {
            gain_db: Some(-3.5),
            muted: Some(true),
            sync_offset_ms: Some(-80),
            ..Default::default()
        };
        let out = audio_set(&state, &ctx(), SceneTarget::Take(take(2)), tid("A2"), patch).unwrap();
        let t = out.state.project.takes[&take(2)]
            .scene
            .track(&tid("A2"))
            .unwrap();
        assert_eq!((t.gain_db, t.muted, t.sync_offset_ms), (-3.5, true, -80));
        assert_eq!(out.state.project.scene, state.project.scene);
        assert_eq!(
            out.state.history.entries[0].label,
            "Change audio track A2 in take 2: gain -3.5 dB, muted, sync offset -80 ms"
        );

        // A new source in the project scene relabels the track.
        let patch = AudioPatch {
            source: Some("alsa_input.pci.analog-stereo".into()),
            ..Default::default()
        };
        let out = audio_set(&state, &ctx(), SceneTarget::Project, tid("A1"), patch).unwrap();
        let a1 = out.state.project.scene.track(&tid("A1")).unwrap();
        assert_eq!(
            (a1.source.as_str(), a1.label.as_str()),
            ("alsa_input.pci.analog-stereo", "Built-in Mic")
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Change audio track A1: source alsa_input.pci.analog-stereo, label \"Built-in Mic\""
        );
        // An explicit label wins; an unknown source keeps the label.
        let patch = AudioPatch {
            source: Some("alsa_input.pci.analog-stereo".into()),
            label: Some("Desk".into()),
            ..Default::default()
        };
        let out = audio_set(&state, &ctx(), SceneTarget::Project, tid("A1"), patch).unwrap();
        assert_eq!(
            out.state.project.scene.track(&tid("A1")).unwrap().label,
            "Desk"
        );
        let patch = AudioPatch {
            source: Some("alsa_input.unknown".into()),
            ..Default::default()
        };
        let out = audio_set(&state, &ctx(), SceneTarget::Project, tid("A1"), patch).unwrap();
        assert_eq!(
            out.state.project.scene.track(&tid("A1")).unwrap().label,
            "Mic"
        );
    }

    #[test]
    fn audio_set_errors() {
        let state = studio();
        let set = |target, track: &str, patch| {
            audio_set(&state, &ctx(), target, tid(track), patch).unwrap_err()
        };
        assert_eq!(
            set(SceneTarget::Project, "A1", AudioPatch::default()).code,
            ErrorCode::Usage
        );
        for gain in [60.5, -61.0, f64::INFINITY] {
            let patch = AudioPatch {
                gain_db: Some(gain),
                ..Default::default()
            };
            assert_eq!(
                set(SceneTarget::Project, "A1", patch).code,
                ErrorCode::Usage,
                "{gain}"
            );
        }
        let patch = AudioPatch {
            sync_offset_ms: Some(5001),
            ..Default::default()
        };
        assert_eq!(
            set(SceneTarget::Project, "A1", patch).code,
            ErrorCode::Usage
        );
        let source = |s: &str| AudioPatch {
            source: Some(s.into()),
            ..Default::default()
        };
        let err = set(
            SceneTarget::Take(take(1)),
            "A1",
            source("alsa_input.pci.analog-stereo"),
        );
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "take 1 recorded A1 from alsa_input.usb-Yeti; a take's sources are facts"
        );
        let err = set(
            SceneTarget::Project,
            "A1",
            source("alsa_output.pci.analog-stereo.monitor"),
        );
        assert_eq!(err.code, ErrorCode::Invalid, "a monitor can't be a mic");
        let err = set(SceneTarget::Project, "A2", source("alsa_input.usb-Yeti"));
        assert_eq!(err.message, "alsa_input.usb-Yeti is already track A1");
        let err = set(SceneTarget::Project, "A9", source("x"));
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.hint.as_deref(), Some("tracks there: A1, A2"));

        // Naming a take's own source again is not a change.
        let out = audio_set(
            &state,
            &ctx(),
            SceneTarget::Take(take(1)),
            tid("A1"),
            source("alsa_input.usb-Yeti"),
        )
        .unwrap();
        assert_eq!(reply(&out).seq, None);
    }

    // ---------------------------------------------------------- audio remove

    #[test]
    fn removing_the_autocut_track_resets_it_in_the_same_entry() {
        let mut state = studio();
        state.project.autocut.track = Some(tid("A1"));
        let out = audio_remove(&state, &ctx(), SceneTarget::Project, tid("A1")).unwrap();
        assert_eq!(track_ids(&out.state.project.scene), ["A2"]);
        assert_eq!(out.state.project.autocut.track, None);
        assert_eq!(out.state.history.entries.len(), 1);
        assert_eq!(
            out.state.history.entries[0].label,
            "Remove audio track A1 (Mic) (autocut track reset)"
        );
        assert_eq!(reply(&out).removed, ["A1"]);
        assert!(
            reply(&out)
                .message
                .ends_with("autocut now uses each take's first mic track")
        );

        // Another track, or a take's snapshot, leaves the setting alone.
        let out = audio_remove(&state, &ctx(), SceneTarget::Project, tid("A2")).unwrap();
        assert_eq!(out.state.project.autocut.track, Some(tid("A1")));
        let out = audio_remove(&state, &ctx(), SceneTarget::Take(take(1)), tid("A1")).unwrap();
        assert_eq!(out.state.project.autocut.track, Some(tid("A1")));
        let t1 = &out.state.project.takes[&take(1)];
        assert_eq!(track_ids(&t1.scene), ["A2"]);
        assert!(t1.stream("A1").is_some(), "the stream stays");
        let err = audio_remove(&state, &ctx(), SceneTarget::Project, tid("A7")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    // ----------------------------------------------------------- scene apply

    /// The studio with take 1's layout edited away from the project's.
    fn diverged() -> State {
        let mut state = studio();
        let t1 = state.project.takes.get_mut(&take(1)).unwrap();
        for layer in &mut t1.scene.layers {
            layer.rect = Rect::new(1, 2, 300, 200);
            layer.opacity = 0.25;
            layer.visible = false;
            layer.fit = Fit::Cover;
            layer.crop.top = 8;
            layer.name = format!("Take {}", layer.name);
            layer.locked = true;
        }
        t1.scene.layers.reverse();
        t1.scene.layers[0].missing = true;
        for track in &mut t1.scene.audio {
            track.gain_db = -12.0;
            track.muted = true;
            track.sync_offset_ms = 40;
            track.label = "Take track".into();
            track.source = format!("{}.then", track.source);
        }
        t1.scene.audio[1].missing = true;
        state
    }

    fn apply_scene(state: &State, from: SceneTarget, to: SceneDest) -> Outcome {
        scene_apply(state, &ctx(), from, to).unwrap()
    }

    #[test]
    fn apply_resets_a_take_to_the_project_layout() {
        // `qrec scene apply --take take-0001`
        let state = diverged();
        let before = state.project.takes[&take(1)].scene.clone();
        let out = apply_scene(
            &state,
            SceneTarget::Project,
            SceneDest::Takes(vec![take(1)]),
        );
        let after = &out.state.project.takes[&take(1)].scene;
        let project = &state.project.scene;
        assert_eq!(layer_ids(after), ["L1", "L2"], "the project's z-order");
        for layer in &after.layers {
            let src = project.layer(&layer.id).unwrap();
            let old = before.layer(&layer.id).unwrap();
            assert_eq!(
                (
                    layer.rect,
                    layer.crop,
                    layer.opacity,
                    layer.visible,
                    layer.fit
                ),
                (src.rect, src.crop, src.opacity, src.visible, src.fit)
            );
            assert_eq!(
                (&layer.name, layer.locked, &layer.source, layer.missing),
                (&old.name, old.locked, &old.source, old.missing),
                "names, locks, sources and missing stay"
            );
        }
        for track in &after.audio {
            let src = project.track(&track.id).unwrap();
            let old = before.track(&track.id).unwrap();
            assert_eq!(
                (track.gain_db, track.muted, track.sync_offset_ms),
                (src.gain_db, src.muted, src.sync_offset_ms)
            );
            assert_eq!(
                (&track.label, &track.source, track.missing),
                (&old.label, &old.source, old.missing)
            );
        }
        assert_eq!(
            out.state.project.takes[&take(2)],
            state.project.takes[&take(2)]
        );
        assert_eq!(out.state.project.scene, state.project.scene);
        assert_eq!(
            out.state.history.entries[0].label,
            "Apply the project layout to take 1"
        );
        assert_eq!(reply(&out).result["scenes"], json!(["take-0001"]));

        // Applying again changes nothing.
        let again = apply_scene(
            &out.state,
            SceneTarget::Project,
            SceneDest::Takes(vec![take(1)]),
        );
        assert_eq!(reply(&again).seq, None);
        assert_eq!(
            reply(&again).message,
            "Nothing changed: take 1 already matches the project layout"
        );
    }

    #[test]
    fn apply_to_all_takes() {
        // `qrec scene apply --to-all-takes`
        let mut state = diverged();
        let t2 = state.project.takes.get_mut(&take(2)).unwrap();
        t2.scene.layers[0].opacity = 0.5;
        let mut recording = t2.clone();
        recording.status = TakeStatus::Recording;
        recording.scene.layers[0].opacity = 0.1;
        state.project.takes.insert(take(3), recording);
        let out = apply_scene(&state, SceneTarget::Project, SceneDest::AllTakes);
        let p = &out.state.project;
        assert_eq!(
            p.takes[&take(1)].scene.layers[0].rect,
            p.scene.layers[0].rect
        );
        assert_eq!(p.takes[&take(2)].scene.layers[0].opacity, 1.0);
        assert_eq!(
            p.takes[&take(3)].scene.layers[0].opacity,
            0.1,
            "a take still recording is skipped"
        );
        assert_eq!(
            reply(&out).message,
            "Applied the project layout to all takes (2 takes)"
        );
        assert_eq!(
            reply(&out).result["scenes"],
            json!(["take-0001", "take-0002"])
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Apply the project layout to all takes"
        );
    }

    #[test]
    fn apply_a_take_layout_to_the_project() {
        // `qrec scene apply --from-take take-0001`
        let mut state = diverged();
        // A project-only layer keeps its slot.
        let mut extra = state.project.scene.layers[0].clone();
        extra.id = lid("L3");
        extra.source = VideoSource::Screen {
            output: "HDMI-A-1".into(),
            label: "HDMI".into(),
        };
        state.project.scene.layers.insert(1, extra);
        let out = apply_scene(&state, SceneTarget::Take(take(1)), SceneDest::Project);
        let scene = &out.state.project.scene;
        assert_eq!(layer_ids(scene), ["L2", "L3", "L1"]);
        let take_scene = &state.project.takes[&take(1)].scene;
        for layer in scene.layers.iter().filter(|l| l.id.as_str() != "L3") {
            let src = take_scene.layer(&layer.id).unwrap();
            assert_eq!(
                (layer.rect, layer.opacity, layer.fit),
                (src.rect, src.opacity, src.fit)
            );
            assert!(!layer.missing, "missing is never copied");
            assert!(!layer.locked && !layer.name.starts_with("Take"));
        }
        for track in &scene.audio {
            assert_eq!(
                (track.gain_db, track.muted, track.sync_offset_ms),
                (-12.0, true, 40)
            );
            assert!(!track.missing && !track.source.ends_with(".then"));
        }
        assert_eq!(
            scene.layer(&lid("L3")).unwrap(),
            &state.project.scene.layers[1]
        );
        assert_eq!(out.state.project.takes, state.project.takes);
        assert_eq!(
            out.state.history.entries[0].label,
            "Apply take 1's layout to the project scene"
        );
    }

    #[test]
    fn apply_a_take_layout_to_all_other_takes() {
        // `qrec scene apply --from-take take-0001 --to-all-takes`
        let state = diverged();
        let out = apply_scene(&state, SceneTarget::Take(take(1)), SceneDest::AllTakes);
        let p = &out.state.project;
        let t1 = &state.project.takes[&take(1)].scene;
        let t2 = &p.takes[&take(2)].scene;
        assert_eq!(layer_ids(t2), ["L2", "L1"]);
        assert_eq!(t2.layers[0].rect, t1.layers[0].rect);
        assert!(!t2.layers[0].missing);
        assert_eq!(t2.audio[1].gain_db, -12.0);
        assert!(
            !t2.audio[1].missing && t2.audio[1].source == "alsa_output.pci.analog-stereo.monitor"
        );
        assert_eq!(p.takes[&take(1)], state.project.takes[&take(1)]);
        assert_eq!(p.scene, state.project.scene);
        assert_eq!(reply(&out).result["scenes"], json!(["take-0002"]));
        assert_eq!(
            out.state.history.entries[0].label,
            "Apply take 1's layout to all takes"
        );
        // And to chosen takes (`--from-take take-0001 --take take-0002`).
        let out = apply_scene(
            &state,
            SceneTarget::Take(take(1)),
            SceneDest::Takes(vec![take(2), take(1)]),
        );
        assert_eq!(
            layer_ids(&out.state.project.takes[&take(2)].scene),
            ["L2", "L1"]
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Apply take 1's layout to take 2"
        );
    }

    #[test]
    fn apply_brings_back_what_a_take_has_media_for() {
        let mut state = studio();
        // L3 (a new camera) joined the project after the takes were recorded.
        let mut extra = state.project.scene.layers[1].clone();
        extra.id = lid("L3");
        state.project.scene.layers.insert(0, extra);
        let t1 = state.project.takes.get_mut(&take(1)).unwrap();
        t1.scene.layers.retain(|l| l.id.as_str() != "L2");
        t1.scene.audio.retain(|t| t.id.as_str() != "A1");
        // A2's file turned out unreadable: removed from the snapshot too.
        t1.scene.audio.clear();
        t1.stream_mut("A2").unwrap().sample_rate = None;
        let out = apply_scene(
            &state,
            SceneTarget::Project,
            SceneDest::Takes(vec![take(1)]),
        );
        let scene = &out.state.project.takes[&take(1)].scene;
        assert_eq!(layer_ids(scene), ["L1", "L2"], "L3 has no stream in take 1");
        let l2 = scene.layer(&lid("L2")).unwrap();
        assert_eq!(l2, state.project.scene.layer(&lid("L2")).unwrap());
        assert_eq!(track_ids(scene), ["A1", "A2"]);
        assert!(!scene.audio[0].missing);
        assert!(
            scene.audio[1].missing,
            "a stream without media comes back missing"
        );
        assert_eq!(
            reply(&out).message,
            "Applied the project layout to take 1; re-added L2, A1, A2 to take 1"
        );
        assert_eq!(
            reply(&out).result["added"],
            json!({"take-0001": ["L2", "A1", "A2"]})
        );
    }

    #[test]
    fn re_added_tracks_keep_the_source_order() {
        let mut state = testkit::state(
            ProjectBuilder::new()
                .mic("A1", "alsa_input.a")
                .mic("A2", "alsa_input.b")
                .mic("A3", "alsa_input.c")
                .take("take-0001", 100)
                .build(),
        );
        let t1 = state.project.takes.get_mut(&take(1)).unwrap();
        t1.scene.audio.retain(|t| t.id.as_str() == "A3");
        let out = apply_scene(&state, SceneTarget::Project, SceneDest::AllTakes);
        assert_eq!(
            track_ids(&out.state.project.takes[&take(1)].scene),
            ["A1", "A2", "A3"]
        );
        let t1 = state.project.takes.get_mut(&take(1)).unwrap();
        t1.scene.audio = state.project.scene.audio[..1].to_vec();
        let out = apply_scene(&state, SceneTarget::Project, SceneDest::AllTakes);
        assert_eq!(
            track_ids(&out.state.project.takes[&take(1)].scene),
            ["A1", "A2", "A3"]
        );
    }

    #[test]
    fn z_order_follows_the_source_around_other_layers() {
        let source = Scene {
            layers: ["L3", "L1", "L4"].map(layer_with).to_vec(),
            audio: vec![],
        };
        let mut dest = Scene {
            layers: ["L1", "L2", "L3"].map(layer_with).to_vec(),
            audio: vec![],
        };
        reorder_layers(&source, &mut dest, vec![layer_with("L4")]);
        assert_eq!(layer_ids(&dest), ["L3", "L2", "L1", "L4"]);

        // Duplicated ids (a broken hand edit) are kept, never dropped.
        let mut dest = Scene {
            layers: ["L1", "L1", "L3"].map(layer_with).to_vec(),
            audio: vec![],
        };
        reorder_layers(&source, &mut dest, vec![]);
        assert_eq!(layer_ids(&dest), ["L3", "L1", "L1"]);
    }

    fn layer_with(id: &str) -> Layer {
        let mut layer = ProjectBuilder::new()
            .screen_layer(id, id)
            .build()
            .scene
            .layers[0]
            .clone();
        layer.name = id.into();
        layer
    }

    #[test]
    fn scene_apply_errors() {
        let state = studio();
        let err = |from, to| scene_apply(&state, &ctx(), from, to).unwrap_err();
        let e = err(SceneTarget::Project, SceneDest::Project);
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(e.message, "the project scene can't be applied to itself");
        assert_eq!(
            err(SceneTarget::Project, SceneDest::Takes(vec![])).code,
            ErrorCode::Usage
        );
        assert_eq!(
            err(SceneTarget::Project, SceneDest::Takes(vec![take(7)])).code,
            ErrorCode::NotFound
        );
        assert_eq!(
            err(SceneTarget::Take(take(7)), SceneDest::Project).code,
            ErrorCode::NotFound
        );
        let e = err(SceneTarget::Take(take(1)), SceneDest::Takes(vec![take(1)]));
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(e.message, "take 1 can't be applied to itself");

        // No takes at all: nothing to do, and no entry.
        let out = scene_apply(&empty(), &ctx(), SceneTarget::Project, SceneDest::AllTakes).unwrap();
        assert_eq!(reply(&out).seq, None);
        assert_eq!(
            reply(&out).message,
            "Nothing changed: there are no takes to apply the project layout to"
        );
        // From the only take to all the others.
        let single = testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-4")
                .take("take-0001", 100)
                .build(),
        );
        let out = scene_apply(
            &single,
            &ctx(),
            SceneTarget::Take(take(1)),
            SceneDest::AllTakes,
        )
        .unwrap();
        assert_eq!(
            reply(&out).message,
            "Nothing changed: there are no other takes to apply take 1's layout to"
        );
    }

    // ------------------------------------------------------------- snapshot

    /// A setup session through `apply`, pinning what people and agents see:
    /// messages, created/removed ids, results and history labels.
    #[test]
    fn scene_session_snapshot() {
        let mut state = testkit::state(
            ProjectBuilder::new()
                .canvas(1920, 1080, Fps::new(30, 1))
                .build(),
        );
        let commands = vec![
            Command::LayerAdd {
                target: SceneTarget::Project,
                source: screen("DP-4"),
                name: None,
                rect: None,
            },
            Command::LayerAdd {
                target: SceneTarget::Project,
                source: camera("/dev/video0"),
                name: Some("Face".into()),
                rect: None,
            },
            Command::AudioAdd {
                target: SceneTarget::Project,
                role: AudioRole::Mic,
                source: None,
                label: None,
            },
            Command::AudioAdd {
                target: SceneTarget::Project,
                role: AudioRole::System,
                source: None,
                label: Some("Desktop".into()),
            },
            Command::LayerSet {
                target: SceneTarget::Project,
                layer: lid("L2"),
                patch: LayerPatch {
                    rect: Some(Rect::new(1500, 760, 384, 288)),
                    opacity: Some(0.9),
                    ..Default::default()
                },
            },
            Command::LayerOrder {
                target: SceneTarget::Project,
                layer: lid("L2"),
                to: 0,
            },
            Command::AudioSet {
                target: SceneTarget::Project,
                track: tid("A2"),
                patch: AudioPatch {
                    gain_db: Some(-6.0),
                    ..Default::default()
                },
            },
            Command::AudioRemove {
                target: SceneTarget::Project,
                track: tid("A2"),
            },
            Command::LayerRemove {
                target: SceneTarget::Project,
                layer: lid("L2"),
            },
        ];
        let mut replies = Vec::new();
        for cmd in commands {
            let out = crate::apply::apply(&state, &ctx(), cmd).unwrap();
            replies.push(reply(&out).clone());
            state = out.state;
        }
        let labels: Vec<&str> = state
            .history
            .entries
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        insta::assert_yaml_snapshot!("scene_session", (replies, labels, &state.project.scene));
    }
}
