//! `query`: read-only answers as JSON (A§4.5). Owner: meta.
//!
//! Views live in `protocol::reply` (`StatusView`, `TakeRow`, `ClipRow`,
//! `HistoryRow`). Queries work in every record phase and while
//! `qrec.json` on disk is invalid; they never change the state.

use serde::Serialize;
use serde_json::{Value, json};

use crate::apply::project::is_initialized;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::TakeId;
use crate::keymap;
use crate::model::{Project, TakeStatus};
use crate::protocol::query::Query;
use crate::protocol::reply::{ClipRow, HistoryRow, StatusView, TakeRow};
use crate::state::State;
use crate::timeline::{clip_row, total_frames};

/// Answers one query (pure).
pub fn query(state: &State, ctx: &Ctx, q: &Query) -> Result<Value, CoreError> {
    match q {
        Query::Status => status(state, ctx),
        Query::State => state_json(state),
        Query::TakeList { unused_only } => take_list(state, *unused_only),
        Query::TakeShow { take } => take_show(state, take),
        Query::ClipList => clip_list(state),
        Query::History { limit } => history(state, *limit),
        Query::Keys => keys(state, ctx),
    }
}

fn to_json(view: &impl Serialize) -> Result<Value, CoreError> {
    serde_json::to_value(view)
        .map_err(|e| CoreError::internal(format!("view doesn't serialize: {e}")))
}

/// `StatusView` (`path`/`pid` are left `None` for the shell to fill).
/// `initialized` is false for the placeholder state of a folder without
/// `qrec.json` (`apply::project::uninitialized_state`).
pub fn status(state: &State, _ctx: &Ctx) -> Result<Value, CoreError> {
    let project = &state.project;
    to_json(&StatusView {
        project: project.name.clone(),
        path: None,
        initialized: is_initialized(project),
        host: state.runtime.host,
        pid: None,
        record: state.runtime.record.clone(),
        jobs: state.runtime.jobs.values().cloned().collect(),
        takes: project.takes.len(),
        clips: project.timeline.clips.len(),
        duration_frames: total_frames(&project.timeline.clips),
        cursor: state.history.cursor,
    })
}

/// The whole `State` as JSON (`qrec status --json`), except that the
/// history keeps its positions (`cursor`, `baseSeq`, `nextSeq`) and lists
/// its entries as [`HistoryRow`]s: the patches and the derived
/// `seenClipIds` would make the reply grow with every edit (`qrec history`
/// and `.qrec/history.jsonl` have them).
pub fn state_json(state: &State) -> Result<Value, CoreError> {
    Ok(json!({
        "project": to_json(&state.project)?,
        "ui": to_json(&state.ui)?,
        "history": {
            "cursor": state.history.cursor,
            "baseSeq": state.history.base_seq,
            "nextSeq": state.history.next_seq,
            "entries": history_rows(state, None),
        },
        "runtime": to_json(&state.runtime)?,
    }))
}

/// How many timeline clips use `take`.
fn clip_count(project: &Project, take: &TakeId) -> usize {
    project
        .timeline
        .clips
        .iter()
        .filter(|c| &c.take == take)
        .count()
}

/// The take is in the bin: no clip uses it and it isn't being recorded.
/// Broken and too-short takes never have clips, so they are always there.
fn in_bin(project: &Project, id: &TakeId) -> bool {
    project
        .take(id)
        .is_some_and(|t| t.status != TakeStatus::Recording && clip_count(project, id) == 0)
}

/// `[TakeRow]` by take number, all takes or only the bin (`unused`).
pub fn take_list(state: &State, unused_only: bool) -> Result<Value, CoreError> {
    let project = &state.project;
    let mut ids: Vec<&TakeId> = project.takes.keys().collect();
    ids.sort_by_key(|id| id.number());
    let rows: Vec<TakeRow> = ids
        .into_iter()
        .filter_map(|id| {
            let take = &project.takes[id];
            let row = TakeRow {
                id: id.clone(),
                status: take.status,
                recorded_at: take.recorded_at,
                duration_frames: take.duration_frames,
                clips: clip_count(project, id),
                unused: in_bin(project, id),
                error: take.error.clone(),
            };
            (!unused_only || row.unused).then_some(row)
        })
        .collect();
    to_json(&rows)
}

/// One take in full (`NotFound` for an unknown id): every field of the
/// take (optional ones written explicitly) plus `id`, `unused` and the
/// clips that use it as `[ClipRow]`.
pub fn take_show(state: &State, take: &TakeId) -> Result<Value, CoreError> {
    let project = &state.project;
    let found = project.take(take).ok_or_else(|| {
        CoreError::not_found(format!("there is no {take}"))
            .with_hint("`qrec take list` shows the takes")
    })?;
    let mut value = to_json(found)?;
    let clips: Vec<ClipRow> = clip_rows(project)
        .into_iter()
        .filter(|row| &row.take == take)
        .collect();
    if let Value::Object(map) = &mut value {
        map.insert("id".into(), json!(take));
        map.insert("error".into(), json!(found.error));
        map.insert("placed".into(), json!(found.placed));
        map.insert("replaced".into(), to_json(&found.replaced)?);
        map.insert("unused".into(), json!(in_bin(project, take)));
        map.insert("clips".into(), to_json(&clips)?);
    }
    Ok(value)
}

/// Every clip's `ClipRow` (`timeline::clip_row`, the row `clip` commands
/// return too), in timeline order.
fn clip_rows(project: &Project) -> Vec<ClipRow> {
    let clips = &project.timeline.clips;
    (0..clips.len()).map(|i| clip_row(clips, i)).collect()
}

/// `[ClipRow]` in timeline order; `start` is the clip's timeline position.
pub fn clip_list(state: &State) -> Result<Value, CoreError> {
    to_json(&clip_rows(&state.project))
}

fn history_rows(state: &State, limit: Option<usize>) -> Vec<HistoryRow> {
    let entries = &state.history.entries;
    let skip = limit.map_or(0, |n| entries.len().saturating_sub(n));
    entries
        .iter()
        .skip(skip)
        .map(|e| HistoryRow {
            seq: e.seq,
            ts: e.ts,
            label: e.label.clone(),
            current: e.seq == state.history.cursor,
        })
        .collect()
}

/// `[HistoryRow]`, the newest `limit` entries (all without a limit), oldest
/// first. Entries after the `current` one are the redo tail; when none is
/// current, everything is undone (the cursor is at the base snapshot).
pub fn history(state: &State, limit: Option<usize>) -> Result<Value, CoreError> {
    to_json(&history_rows(state, limit))
}

/// The keymap as `qrec keys --json` prints it (`keymap::keys_view`).
///
/// `Facts` carries no `keys.toml` text yet, so this is the default keymap;
/// the CLI's project-less `qrec keys` resolves the user's file itself.
/// The effective keymap: defaults plus `keys.toml` overrides (`Facts::keys_toml`).
pub fn keys(_state: &State, ctx: &Ctx) -> Result<Value, CoreError> {
    to_json(&keymap::keys_view(&keymap::resolve(&ctx.facts.keys_toml)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryEntry;
    use crate::ids::{ClipId, Frame, JobId};
    use crate::state::{HostKind, Job, JobKind, RecordMode, RecordPhase};
    use crate::testkit::{self, ProjectBuilder};
    use pretty_assertions::assert_eq;

    fn state() -> State {
        let project = ProjectBuilder::new()
            .name("talk")
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .take("take-0003", 5)
            .with_take("take-0003", |t| t.status = TakeStatus::TooShort)
            .take("take-0004", 120)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 150, 300)
            .clip("c003", "take-0004", 20, 120)
            .build();
        testkit::state(project)
    }

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T13:00:00Z", 1)
    }

    fn entry(seq: u64, label: &str) -> HistoryEntry {
        HistoryEntry {
            seq,
            ts: "2026-10-14T12:30:00Z".parse().unwrap(),
            label: label.into(),
            cmd: json!({"cmd": "clipRemove"}),
            forward: json_patch::Patch(vec![]),
            inverse: json_patch::Patch(vec![]),
            after_hash: String::new(),
        }
    }

    #[test]
    fn status_summarizes() {
        let mut s = state();
        s.runtime.host = HostKind::Headless;
        s.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(5),
            mode: RecordMode::Append,
            elapsed_ns: 2_000_000_000,
            waiters: vec![],
        };
        s.runtime.jobs.insert(
            JobId(3),
            Job {
                id: JobId(3),
                kind: JobKind::FrameSave {
                    out: "/p/frames/x.png".into(),
                    files: Default::default(),
                },
                req: None,
                started: ctx().now,
            },
        );
        s.history.cursor = 7;
        let value = query(&s, &ctx(), &Query::Status).unwrap();
        let view: StatusView = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(view.project, "talk");
        assert_eq!((view.path, view.pid), (None, None));
        assert!(view.initialized);
        assert_eq!(view.host, HostKind::Headless);
        assert_eq!(view.takes, 4);
        assert_eq!(view.clips, 3);
        assert_eq!(view.duration_frames, 350);
        assert_eq!(view.cursor, 7);
        assert_eq!(view.jobs.len(), 1);
        assert_eq!(value["record"]["phase"], "recording");
        assert_eq!(value["durationFrames"], 350);
    }

    #[test]
    fn takes_and_the_bin() {
        let mut s = state();
        s.project.takes.get_mut(&TakeId::numbered(2)).unwrap().error = None;
        let rows: Vec<TakeRow> = serde_json::from_value(
            query(&s, &ctx(), &Query::TakeList { unused_only: false }).unwrap(),
        )
        .unwrap();
        let summary: Vec<(String, usize, bool)> = rows
            .iter()
            .map(|r| (r.id.to_string(), r.clips, r.unused))
            .collect();
        assert_eq!(
            summary,
            [
                ("take-0001".into(), 2, false),
                ("take-0002".into(), 0, true),
                ("take-0003".into(), 0, true),
                ("take-0004".into(), 1, false),
            ]
        );
        assert_eq!(rows[2].status, TakeStatus::TooShort);
        let bin = query(&s, &ctx(), &Query::TakeList { unused_only: true }).unwrap();
        let ids: Vec<&str> = bin
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["take-0002", "take-0003"]);

        // A take being recorded is not in the bin.
        s.project
            .takes
            .get_mut(&TakeId::numbered(2))
            .unwrap()
            .status = TakeStatus::Recording;
        let bin = query(&s, &ctx(), &Query::TakeList { unused_only: true }).unwrap();
        assert_eq!(bin.as_array().unwrap().len(), 1);
    }

    #[test]
    fn take_numbers_sort_numerically() {
        let mut s = state();
        let take = s.project.takes[&TakeId::numbered(2)].clone();
        s.project.takes.insert(TakeId::numbered(10_000), take);
        let rows = take_list(&s, false).unwrap();
        let ids: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "take-0001",
                "take-0002",
                "take-0003",
                "take-0004",
                "take-10000"
            ]
        );
    }

    #[test]
    fn take_show_is_complete() {
        let s = state();
        let value = query(
            &s,
            &ctx(),
            &Query::TakeShow {
                take: TakeId::numbered(1),
            },
        )
        .unwrap();
        assert_eq!(value["id"], "take-0001");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["durationFrames"], 300);
        assert_eq!(value["placed"], false);
        assert_eq!(value["error"], Value::Null);
        assert_eq!(value["replaced"], json!([]));
        assert_eq!(value["unused"], false);
        assert_eq!(value["streams"].as_array().unwrap().len(), 2);
        assert_eq!(value["scene"]["layers"][0]["id"], "L1");
        assert_eq!(
            value["clips"],
            json!([
                {"id": "c001", "take": "take-0001", "in": 0, "out": 100, "start": 0, "duration": 100},
                {"id": "c002", "take": "take-0001", "in": 150, "out": 300, "start": 100, "duration": 150},
            ])
        );
        let err = query(
            &s,
            &ctx(),
            &Query::TakeShow {
                take: TakeId::numbered(9),
            },
        )
        .unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::NotFound);
        assert_eq!(err.message, "there is no take-0009");
    }

    #[test]
    fn clips_have_timeline_positions() {
        let rows: Vec<ClipRow> =
            serde_json::from_value(query(&state(), &ctx(), &Query::ClipList).unwrap()).unwrap();
        let positions: Vec<(&str, Frame, Frame)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.start, r.duration))
            .collect();
        assert_eq!(
            positions,
            [("c001", 0, 100), ("c002", 100, 150), ("c003", 250, 100)]
        );
        assert_eq!(rows[2].in_, 20);
        assert_eq!(rows[2].out, 120);
    }

    #[test]
    fn history_rows_mark_the_cursor() {
        let mut s = state();
        for (seq, label) in [
            (1, "Split clip c001"),
            (2, "Remove clip c009"),
            (4, "Autocut take 1"),
        ] {
            s.history.append(entry(seq, label));
        }
        s.history.cursor = 2;
        let rows: Vec<HistoryRow> =
            serde_json::from_value(query(&s, &ctx(), &Query::History { limit: None }).unwrap())
                .unwrap();
        let summary: Vec<(u64, bool)> = rows.iter().map(|r| (r.seq, r.current)).collect();
        assert_eq!(summary, [(1, false), (2, true), (4, false)]);
        assert_eq!(rows[0].label, "Split clip c001");
        let rows = history(&s, Some(2)).unwrap();
        let seqs: Vec<u64> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [2, 4], "the newest entries, oldest first");
        assert_eq!(history(&s, Some(0)).unwrap(), json!([]));
        assert_eq!(history(&s, Some(99)).unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn state_keeps_everything_but_the_patches() {
        let mut s = state();
        s.history.append(entry(1, "Split clip c001"));
        s.history.remember_clip_ids([&ClipId::from_raw("c0ff")]);
        let value = query(&s, &ctx(), &Query::State).unwrap();
        assert_eq!(
            serde_json::from_value::<Project>(value["project"].clone()).unwrap(),
            s.project
        );
        assert_eq!(value["ui"], serde_json::to_value(&s.ui).unwrap());
        assert_eq!(value["runtime"], serde_json::to_value(&s.runtime).unwrap());
        assert_eq!(
            value["history"],
            json!({
                "cursor": 1, "baseSeq": 0, "nextSeq": 2,
                "entries": [{"seq": 1, "ts": "2026-10-14T12:30:00Z", "label": "Split clip c001", "current": true}]
            })
        );
    }

    #[test]
    fn keys_are_the_keymap_view() {
        let value = query(&state(), &ctx(), &Query::Keys).unwrap();
        let view: Vec<keymap::BindingView> = serde_json::from_value(value).unwrap();
        assert_eq!(view, keymap::keys_view(&keymap::resolve("")));
        assert!(view.iter().any(|b| b.id == "clip.split" && b.keys == ["s"]));
    }

    #[test]
    fn keys_apply_the_users_overrides() {
        let mut ctx = ctx();
        ctx.facts.keys_toml = "[bindings]\n\"clip.split\" = \"ctrl+b\"\n".into();
        let value = query(&state(), &ctx, &Query::Keys).unwrap();
        let view: Vec<keymap::BindingView> = serde_json::from_value(value).unwrap();
        let split = view.iter().find(|b| b.id == "clip.split").unwrap();
        assert_eq!(split.keys, ["ctrl+b"]);
    }

    #[test]
    fn status_of_a_folder_without_a_project() {
        let state = crate::apply::project::uninitialized_state(HostKind::Cli);
        let view: StatusView =
            serde_json::from_value(query(&state, &ctx(), &Query::Status).unwrap()).unwrap();
        assert!(!view.initialized);
        assert_eq!(view.host, HostKind::Cli);
        assert_eq!((view.takes, view.clips, view.duration_frames), (0, 0, 0));
        assert_eq!(view.cursor, 0);
        assert_eq!(view.record, RecordPhase::Idle);
    }

    #[test]
    fn clip_rows_are_the_timeline_rows() {
        let s = state();
        let rows: Vec<ClipRow> =
            serde_json::from_value(query(&s, &ctx(), &Query::ClipList).unwrap()).unwrap();
        let clips = &s.project.timeline.clips;
        let expected: Vec<ClipRow> = (0..clips.len()).map(|i| clip_row(clips, i)).collect();
        assert_eq!(rows, expected);
        assert_eq!(
            query(
                &testkit::state(ProjectBuilder::new().build()),
                &ctx(),
                &Query::ClipList
            )
            .unwrap(),
            json!([])
        );
    }

    #[test]
    fn queries_work_while_recording_and_with_a_bad_file() {
        let mut s = state();
        s.runtime.disk_invalid = Some("expected value at line 1".into());
        s.runtime.record = RecordPhase::Finalizing {
            take: TakeId::numbered(5),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        for q in [
            Query::Status,
            Query::State,
            Query::TakeList { unused_only: true },
            Query::TakeShow {
                take: TakeId::numbered(1),
            },
            Query::ClipList,
            Query::History { limit: Some(5) },
            Query::Keys,
        ] {
            assert!(query(&s, &ctx(), &q).is_ok(), "{q:?}");
        }
    }
}
