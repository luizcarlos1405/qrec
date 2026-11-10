//! Saved scenes (SPEC §5.6, A§9.11): save, use, rename, duplicate, delete,
//! apply to a take, reload. Owner: scene.
//!
//! The library comes in `ctx.facts.saved_scenes` (read just before the
//! command) and goes out as `Effect::WriteScenes`; `runtime.saved_scenes`
//! keeps the copy the views read, and `ui.scene` the scene last used or
//! saved. Only `SceneUse` and `SceneApplySaved` change the project (one
//! history entry each); the rest change the library and the UI state.

use serde_json::json;

use crate::apply::Outcome;
use crate::apply::project::require_project;
use crate::ctx::{self, Ctx};
use crate::error::CoreError;
use crate::history::commit;
use crate::ids::TakeId;
use crate::model::TakeStatus;
use crate::protocol::command::Command;
use crate::protocol::effect::Effect;
use crate::protocol::reply::CmdResult;
use crate::scenes::{self, SavedScenes, ScenesFile};
use crate::state::State;

/// The library as read for this command (the runtime copy when the shell
/// gave none: tests, previews).
fn library<'a>(state: &'a State, ctx: &'a Ctx) -> &'a ScenesFile {
    ctx.facts
        .saved_scenes
        .as_ref()
        .unwrap_or(&state.runtime.saved_scenes)
}

fn no_scene(name: &str) -> CoreError {
    CoreError::not_found(format!("no saved scene named {name:?}"))
        .with_hint("`qrec scene list` lists the saved scenes")
}

/// A library-only change: `lib` written and kept, `ui.scene` set.
fn library_change(
    state: &State,
    ctx: &Ctx,
    cmd: &Command,
    lib: SavedScenes,
    ui_scene: Option<String>,
    result: CmdResult,
) -> Outcome {
    let mut next = state.clone();
    next.runtime.saved_scenes = ScenesFile::Read(lib.clone());
    next.ui.scene = ui_scene;
    let mut committed = commit(state, ctx, cmd.name().to_owned(), cmd.to_json(), next);
    committed.effects.push(Effect::WriteScenes(lib));
    Outcome::done(committed, result)
}

/// `qrec scene save [<name>]`: the project scene stored under `name` (a
/// name that exists needs confirmation and is replaced in place), or over
/// the saved scene in use.
pub fn scene_save(state: &State, ctx: &Ctx, name: Option<String>) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneSave { name: name.clone() };
    let file = library(state, ctx);
    let mut lib = file.writable()?.clone();
    let project = &state.project;
    let name = match name {
        Some(name) => scenes::clean_name(&name)?,
        None => {
            let status = scenes::status(
                &lib.scenes,
                &project.scene,
                project.autocut.track.as_ref(),
                state.ui.scene.as_deref(),
            );
            status.name.ok_or_else(|| {
                CoreError::invalid("no saved scene is in use")
                    .with_hint("give it a name: `qrec scene save <name>`")
            })?
        }
    };
    let saved = scenes::saved_from(
        name.clone(),
        ctx.now,
        &project.scene,
        project.autocut.track.as_ref(),
    );
    let replaced = lib.find(&name);
    let text = match replaced {
        Some(i) => {
            // Saving changes (no name) needs no question; a name that
            // exists does, even the one in use (the Save as dialog's rule).
            if !(ctx.confirmed || cmd_has_no_name(&cmd)) {
                return Err(CoreError::confirmation_required(format!(
                    "replace the saved scene {:?}",
                    lib.scenes[i].name
                ))
                .with_details(json!({ "replaces": lib.scenes[i].name })));
            }
            lib.scenes[i] = saved;
            format!("Saved changes to scene {name:?}")
        }
        None => {
            lib.scenes.push(saved);
            format!("Saved scene {name:?}")
        }
    };
    let result = CmdResult::new(text).with_result(&json!({ "name": name }));
    Ok(library_change(state, ctx, &cmd, lib, Some(name), result))
}

fn cmd_has_no_name(cmd: &Command) -> bool {
    matches!(cmd, Command::SceneSave { name: None })
}

/// `qrec scene use <name> | --revert`: the saved scene becomes the project
/// scene (one history entry). Sources the project scene has keep their ids.
pub fn scene_use(state: &State, ctx: &Ctx, name: Option<String>) -> Result<Outcome, CoreError> {
    let cmd = Command::SceneUse { name: name.clone() };
    let file = library(state, ctx);
    let name = match name {
        Some(name) => name,
        None => state.ui.scene.clone().ok_or_else(|| {
            CoreError::invalid("no saved scene is in use to revert to")
                .with_hint("`qrec scene use <name>` picks one")
        })?,
    };
    let Some(i) = scenes::find(file.scenes(), &name) else {
        if let ScenesFile::Unreadable(_) = file {
            file.writable()?;
        }
        return Err(no_scene(&name));
    };
    let saved = &file.scenes()[i];
    let project = &state.project;
    let (scene, autocut) = scenes::into_project(
        saved,
        &project.scene,
        ctx::next_layer_id(state),
        ctx::next_track_id(state),
    );
    let mut next = state.clone();
    next.project.scene = scene;
    next.project.autocut.track = autocut;
    next.ui.scene = Some(saved.name.clone());
    next.ui.selected_layer = None;
    if let ScenesFile::Read(_) = file {
        next.runtime.saved_scenes = file.clone();
    }
    let label = format!("Use scene {}", saved.name);
    let result = CmdResult::new(format!("Using scene {:?}", saved.name))
        .with_result(&json!({ "name": saved.name }));
    let committed = commit(state, ctx, label, cmd.to_json(), next);
    Ok(Outcome::done(committed, result))
}

/// `qrec scene rename <name> <to>`.
pub fn scene_rename(
    state: &State,
    ctx: &Ctx,
    name: String,
    to: String,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneRename {
        name: name.clone(),
        to: to.clone(),
    };
    let mut lib = library(state, ctx).writable()?.clone();
    let i = lib.find(&name).ok_or_else(|| no_scene(&name))?;
    let to = scenes::clean_name(&to)?;
    if let Some(j) = lib.find(&to)
        && j != i
    {
        return Err(CoreError::invalid(format!(
            "a saved scene is already named {:?}",
            lib.scenes[j].name
        )));
    }
    let old = std::mem::replace(&mut lib.scenes[i].name, to.clone());
    let follows = state
        .ui
        .scene
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case(&old));
    let ui_scene = if follows {
        Some(to.clone())
    } else {
        state.ui.scene.clone()
    };
    let result = CmdResult::new(format!("Renamed scene {old:?} to {to:?}"))
        .with_result(&json!({ "name": to }));
    Ok(library_change(state, ctx, &cmd, lib, ui_scene, result))
}

/// `qrec scene duplicate <name> [--as <to>]`.
pub fn scene_duplicate(
    state: &State,
    ctx: &Ctx,
    name: String,
    to: Option<String>,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneDuplicate {
        name: name.clone(),
        to: to.clone(),
    };
    let mut lib = library(state, ctx).writable()?.clone();
    let i = lib.find(&name).ok_or_else(|| no_scene(&name))?;
    let to = match to {
        Some(to) => {
            let to = scenes::clean_name(&to)?;
            if let Some(j) = lib.find(&to) {
                return Err(CoreError::invalid(format!(
                    "a saved scene is already named {:?}",
                    lib.scenes[j].name
                )));
            }
            to
        }
        None => scenes::copy_name(&lib.scenes, &lib.scenes[i].name),
    };
    let mut copy = lib.scenes[i].clone();
    copy.name = to.clone();
    copy.saved_at = ctx.now;
    lib.scenes.insert(i + 1, copy);
    let result = CmdResult::new(format!(
        "Duplicated scene {:?} as {to:?}",
        lib.scenes[i].name
    ))
    .with_result(&json!({ "name": to }));
    let ui_scene = state.ui.scene.clone();
    Ok(library_change(state, ctx, &cmd, lib, ui_scene, result))
}

/// `qrec scene rm <name> --yes`: needs confirmation (outside the undo
/// history); the project and its takes keep their layouts.
pub fn scene_delete(state: &State, ctx: &Ctx, name: String) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneDelete { name: name.clone() };
    let mut lib = library(state, ctx).writable()?.clone();
    let i = lib.find(&name).ok_or_else(|| no_scene(&name))?;
    let gone = lib.scenes[i].name.clone();
    if !ctx.confirmed {
        return Err(CoreError::confirmation_required(format!(
            "delete the saved scene {gone:?} from this computer"
        ))
        .with_details(json!({ "scene": gone })));
    }
    lib.scenes.remove(i);
    let ui_scene = state
        .ui
        .scene
        .clone()
        .filter(|s| !s.eq_ignore_ascii_case(&gone));
    let result = CmdResult::new(format!("Deleted scene {gone:?}"));
    Ok(library_change(state, ctx, &cmd, lib, ui_scene, result))
}

/// `qrec scene apply --scene <name> --take <take>` (one history entry).
pub fn scene_apply_saved(
    state: &State,
    ctx: &Ctx,
    name: String,
    take: TakeId,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::SceneApplySaved {
        name: name.clone(),
        take: take.clone(),
    };
    let file = library(state, ctx);
    let i = scenes::find(file.scenes(), &name).ok_or_else(|| no_scene(&name))?;
    let saved = &file.scenes()[i];
    let Some(t) = state.project.take(&take) else {
        return Err(CoreError::not_found(format!("no take {take}")));
    };
    if t.status == TakeStatus::Recording {
        return Err(CoreError::invalid(format!(
            "{take} is still recording; its layout can't change yet"
        )));
    }
    let (scene, matched) = scenes::onto_take(saved, &t.scene);
    if matched == 0 {
        return Err(CoreError::invalid(format!(
            "{take} shares no layer with scene {:?}",
            saved.name
        ))
        .with_hint("a take keeps the sources it recorded; pick a scene with one of them"));
    }
    let mut next = state.clone();
    if let Some(t) = next.project.takes.get_mut(&take) {
        t.scene = scene;
    }
    let label = format!("Apply scene {} to take {}", saved.name, take.number());
    let result = CmdResult::new(format!(
        "Applied scene {:?} to take {}",
        saved.name,
        take.number()
    ));
    let committed = commit(state, ctx, label, cmd.to_json(), next);
    Ok(Outcome::done(committed, result))
}

/// `scenes.json` changed under a window: keep the new copy.
pub fn scenes_reload(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    let mut next = state.clone();
    if let Some(file) = &ctx.facts.saved_scenes {
        next.runtime.saved_scenes = file.clone();
    }
    Ok(Outcome::new(next, Vec::new(), Some(Ok(CmdResult::new("")))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use crate::ids::{LayerId, TrackId};
    use crate::model::{AudioRole, AudioTrack, Crop, Fit, Layer, Rect, Scene, VideoSource};
    use crate::testkit;

    fn camera(id: u32, rect: Rect) -> Layer {
        Layer {
            id: LayerId::numbered(id),
            name: "Face".into(),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Camera {
                device: "/dev/video0".into(),
                label: "Logitech C920".into(),
            },
            rect,
            crop: Crop::default(),
            fit: Fit::Cover,
            locked: false,
            missing: false,
        }
    }

    fn screen(id: u32) -> Layer {
        Layer {
            id: LayerId::numbered(id),
            name: "Main screen".into(),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Screen {
                output: "DP-4".into(),
                label: "GIGA-BYTE".into(),
            },
            rect: Rect::new(0, 0, 1920, 1080),
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        }
    }

    fn mic(id: u32) -> AudioTrack {
        AudioTrack {
            id: TrackId::numbered(id),
            role: AudioRole::Mic,
            source: "alsa_input.yeti".into(),
            label: "Yeti Nano".into(),
            gain_db: 0.0,
            muted: false,
            sync_offset_ms: 0,
            missing: false,
        }
    }

    const CORNER: Rect = Rect::new(1500, 760, 384, 288);

    fn project_state() -> State {
        let mut state = testkit::design_state();
        state.project.scene = Scene {
            layers: vec![screen(1), camera(2, CORNER)],
            audio: vec![mic(1)],
        };
        state
    }

    fn ctx_with(lib: &SavedScenes, confirmed: bool) -> Ctx {
        let mut ctx = testkit::ctx(testkit::CREATED_AT, 7);
        ctx.confirmed = confirmed;
        ctx.facts.saved_scenes = Some(ScenesFile::Read(lib.clone()));
        ctx
    }

    fn written(outcome: &Outcome) -> SavedScenes {
        outcome
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::WriteScenes(lib) => Some(lib.clone()),
                _ => None,
            })
            .expect("scenes.json is written")
    }

    fn names(lib: &SavedScenes) -> Vec<&str> {
        lib.scenes.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn saving_adds_a_scene_and_asks_before_replacing_one() {
        let state = project_state();
        let empty = SavedScenes::default();
        let out = scene_save(&state, &ctx_with(&empty, false), Some(" Talk ".into())).unwrap();
        let lib = written(&out);
        assert_eq!(names(&lib), ["Talk"]);
        assert_eq!(out.state.ui.scene.as_deref(), Some("Talk"));
        assert_eq!(out.state.history, state.history, "not in the undo history");

        let mut other = out.state.clone();
        other.ui.scene = None;
        let err = scene_save(&other, &ctx_with(&lib, false), Some("talk".into())).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        let out = scene_save(&other, &ctx_with(&lib, true), Some("talk".into())).unwrap();
        assert_eq!(names(&written(&out)), ["talk"], "replaced in place");
    }

    #[test]
    fn saving_changes_goes_to_the_scene_in_use() {
        let state = project_state();
        let lib = written(
            &scene_save(
                &state,
                &ctx_with(&SavedScenes::default(), false),
                Some("Talk".into()),
            )
            .unwrap(),
        );
        let mut moved = state.clone();
        moved.ui.scene = Some("Talk".into());
        moved.project.scene.layers[1].rect.x = 1180;
        let out = scene_save(&moved, &ctx_with(&lib, false), None).unwrap();
        assert_eq!(written(&out).scenes[0].scene.layers[1].rect.x, 1180);
        let mut other = project_state();
        other.project.scene.layers.pop();
        let none = scene_save(&other, &ctx_with(&lib, false), None);
        assert!(none.is_err(), "no saved scene is in use");
    }

    #[test]
    fn using_a_scene_is_one_undo_step_and_keeps_known_ids() {
        let state = project_state();
        let only_camera = SavedScenes {
            version: scenes::VERSION,
            scenes: vec![scenes::saved_from(
                "Camera only".into(),
                testkit::ctx(testkit::CREATED_AT, 7).now,
                &Scene {
                    layers: vec![camera(9, Rect::new(0, 0, 1920, 1080))],
                    audio: vec![mic(4)],
                },
                Some(&TrackId::numbered(4)),
            )],
        };
        let out = scene_use(
            &state,
            &ctx_with(&only_camera, false),
            Some("camera only".into()),
        )
        .unwrap();
        let scene = &out.state.project.scene;
        assert_eq!(scene.layers.len(), 1);
        assert_eq!(
            scene.layers[0].id,
            LayerId::numbered(2),
            "the camera keeps L2"
        );
        assert_eq!(out.state.project.autocut.track, Some(TrackId::numbered(1)));
        assert_eq!(out.state.ui.scene.as_deref(), Some("Camera only"));
        assert_eq!(
            out.state.history.entries.len(),
            state.history.entries.len() + 1
        );
        let missing = scene_use(&state, &ctx_with(&only_camera, false), Some("Nope".into()));
        assert_eq!(missing.unwrap_err().code, ErrorCode::NotFound);
    }

    #[test]
    fn rename_follows_and_delete_asks_then_forgets() {
        let mut state = project_state();
        state.ui.scene = Some("Talk".into());
        let lib = SavedScenes {
            version: scenes::VERSION,
            scenes: vec![
                scenes::saved_from(
                    "Talk".into(),
                    testkit::ctx(testkit::CREATED_AT, 7).now,
                    &state.project.scene,
                    None,
                ),
                scenes::saved_from(
                    "Other".into(),
                    testkit::ctx(testkit::CREATED_AT, 7).now,
                    &Scene::default(),
                    None,
                ),
            ],
        };
        let out = scene_rename(
            &state,
            &ctx_with(&lib, false),
            "talk".into(),
            "Talk 2".into(),
        )
        .unwrap();
        assert_eq!(out.state.ui.scene.as_deref(), Some("Talk 2"));
        let taken = scene_rename(
            &state,
            &ctx_with(&lib, false),
            "Talk".into(),
            "other".into(),
        );
        assert!(taken.is_err());
        let dup = scene_duplicate(&state, &ctx_with(&lib, false), "Talk".into(), None).unwrap();
        assert_eq!(names(&written(&dup)), ["Talk", "Talk copy", "Other"]);
        let ask = scene_delete(&state, &ctx_with(&lib, false), "Talk".into()).unwrap_err();
        assert_eq!(ask.code, ErrorCode::ConfirmationRequired);
        let out = scene_delete(&state, &ctx_with(&lib, true), "Talk".into()).unwrap();
        assert_eq!(names(&written(&out)), ["Other"]);
        assert_eq!(out.state.ui.scene, None);
        assert_eq!(out.state.project, state.project, "the project is untouched");
    }

    #[test]
    fn an_unreadable_library_is_never_written() {
        let state = project_state();
        let mut ctx = testkit::ctx(testkit::CREATED_AT, 7);
        ctx.facts.saved_scenes = Some(ScenesFile::Unreadable("bad JSON".into()));
        let err = scene_save(&state, &ctx, Some("Talk".into())).unwrap_err();
        assert!(err.message.contains("scenes.json can't be read"), "{err}");
    }
}
