//! Store tests: the A§7.6 commit order, crash points between its steps,
//! torn history lines, rewrites, projects without a commit record,
//! recovery and the never-overwrite rules.

use std::fs;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use qrec_core::ctx::{Ctx, Facts};
use qrec_core::history::ops::unchecked;
use qrec_core::model::Project;
use qrec_core::protocol::{Effect, HistoryOp};
use qrec_core::state::{History, HostKind, State, UiState};
use qrec_core::{ClipId, ErrorCode};

use super::testdata::sample_project;
use super::*;
use crate::paths::ProjectPaths;

fn ctx() -> Ctx {
    Ctx {
        now: "2026-10-14T12:30:00Z".parse::<DateTime<Utc>>().unwrap(),
        utc_offset_secs: 0,
        entropy: 1,
        confirmed: false,
        req: None,
        facts: Facts::default(),
    }
}

fn state(project: Project) -> State {
    State::new(project, History::new(), UiState::default(), HostKind::Cli)
}

/// One undoable edit through the core's commit helper: the next state and
/// the persistence effects it asks for.
fn edit(
    prev: &State,
    label: &str,
    change: impl FnOnce(&mut Project),
) -> (State, Vec<HistoryOp>, bool) {
    let mut next = prev.clone();
    change(&mut next.project);
    let committed = qrec_core::history::commit(
        prev,
        &ctx(),
        label.to_owned(),
        serde_json::json!({"cmd": "test"}),
        next,
    );
    assert!(committed.seq.is_some(), "{label} changed nothing undoable");
    let ops = committed
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::History(op) => Some(op.clone()),
            _ => None,
        })
        .collect();
    let project = committed.effects.contains(&Effect::WriteProject);
    (committed.state, ops, project)
}

fn commit(
    store: &mut Store,
    prev: &State,
    next: &State,
    ops: &[HistoryOp],
    project: bool,
) -> Result<(), CommitError> {
    let refs: Vec<&HistoryOp> = ops.iter().collect();
    store.commit(prev, next, &refs, project)
}

fn opts() -> LoadOpts {
    LoadOpts {
        recovery: false,
        check: unchecked,
    }
}

fn recovery() -> LoadOpts {
    LoadOpts {
        recovery: true,
        check: unchecked,
    }
}

/// A fresh project folder with `.qrec/` and an initial commit.
fn initialized() -> (tempfile::TempDir, Store, State) {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".qrec")).unwrap();
    let mut store = Store::new(ProjectPaths::new(tmp.path()), true);
    let state = state(sample_project());
    store.write_all(&state).unwrap();
    (tmp, store, state)
}

fn remove_first_clip(p: &mut Project) {
    p.timeline.clips.remove(0);
}

fn trim(out: u64) -> impl FnOnce(&mut Project) {
    move |p: &mut Project| p.timeline.clips[0].out = out
}

fn history_lines(store: &Store) -> Vec<u64> {
    let bytes = fs::read(store.paths().history_file()).unwrap_or_default();
    format::parse_history(&bytes)
        .unwrap()
        .entries
        .iter()
        .map(|e| e.seq)
        .collect()
}

#[test]
fn a_first_write_stores_project_head_and_base() {
    let (tmp, store, state) = initialized();
    let paths = store.paths();
    let bytes = fs::read(paths.project_file()).unwrap();
    assert_eq!(bytes, format::project_bytes(&state.project));
    let head: Head = serde_json::from_slice(&fs::read(paths.history_head()).unwrap()).unwrap();
    assert_eq!((head.cursor, head.base_seq, head.next_seq), (0, 0, 1));
    assert_eq!(head.project_sha256, format::sha(&bytes));
    assert_eq!(head.project, state.project);
    assert_eq!(store.last_sha(), Some(head.project_sha256.as_str()));
    let base: BaseFile = serde_json::from_slice(&fs::read(paths.history_base()).unwrap()).unwrap();
    assert_eq!((base.base_seq, &base.project), (0, &state.project));
    assert!(!paths.history_file().exists() || history_lines(&store).is_empty());

    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, state.project);
    assert_eq!(loaded.external, None);
    assert!(!loaded.needs_repair, "{:?}", loaded.notes);
}

#[test]
fn appends_commit_in_order_and_reload() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    assert!(matches!(ops.as_slice(), [HistoryOp::Append(_)]));
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let (s2, ops, project) = edit(&s1, "Trim", trim(200));
    commit(&mut store, &s1, &s2, &ops, project).unwrap();
    assert_eq!(history_lines(&store), [1, 2]);

    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, s2.project);
    assert_eq!(loaded.history.entries, s2.history.entries);
    assert_eq!(
        (
            loaded.history.cursor,
            loaded.history.base_seq,
            loaded.history.next_seq
        ),
        (2, 0, 3)
    );
    assert!(
        loaded
            .history
            .seen_clip_ids
            .contains(&ClipId::from_raw("c001"))
    );
    assert_eq!(loaded.external, None);
    assert!(!loaded.needs_repair);
}

#[test]
fn a_crash_after_the_history_line_drops_it() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    store.crash_after = Some(Step::History);
    assert!(matches!(
        commit(&mut store, &s0, &s1, &ops, project),
        Err(CommitError::Crashed(Step::History))
    ));
    assert_eq!(history_lines(&store), [1], "the line is on disk…");

    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, s0.project, "…but was never committed");
    assert!(loaded.history.entries.is_empty());
    assert_eq!((loaded.history.cursor, loaded.history.next_seq), (0, 1));
    assert_eq!(loaded.external, None);
    assert!(loaded.needs_repair, "the file gets cleaned");

    // The repaired store reuses the seq without a duplicate line.
    let mut store = Store::new(ProjectPaths::new(tmp.path()), true);
    store.adopt(&loaded);
    let reopened = state(loaded.project.clone());
    let reopened = State {
        history: loaded.history.clone(),
        ..reopened
    };
    let (s1b, ops, project) = edit(&reopened, "Trim", trim(90));
    assert_eq!(s1b.history.cursor, 1);
    commit(&mut store, &reopened, &s1b, &ops, project).unwrap();
    assert_eq!(history_lines(&store), [1]);
    let again = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(again.project, s1b.project);
    assert_eq!(again.history.entries[0].label, "Trim");
}

/// A crash between writing `qrec.json` and the head (A§7.6 steps 2 and
/// 3): the file is that commit's own write, so the open finishes the
/// commit (its line kept, the head rewritten by the repair) instead of
/// merging the file as an edit.
#[test]
fn a_crash_after_writing_qrec_json_finishes_the_commit() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    store.crash_after = Some(Step::Project);
    assert!(matches!(
        commit(&mut store, &s0, &s1, &ops, project),
        Err(CommitError::Crashed(Step::Project))
    ));

    let paths = ProjectPaths::new(tmp.path());
    let loaded = load(&paths, &opts()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!(loaded.history.entries, s1.history.entries);
    assert_eq!(
        (loaded.history.cursor, loaded.history.next_seq),
        (1, 2),
        "{:?}",
        loaded.notes
    );
    assert_eq!(loaded.external, None);
    assert!(loaded.needs_repair, "the head gets written");
    assert_eq!(
        loaded.accepted_sha.as_deref(),
        Some(format::sha(&format::project_bytes(&s1.project)).as_str())
    );
    assert!(
        loaded.notes.iter().any(|n| n.contains("Remove c001")),
        "{:?}",
        loaded.notes
    );

    // The repair commits it: the next load is clean.
    let mut store = Store::new(paths.clone(), true);
    store.adopt(&loaded);
    let mut reopened = state(loaded.project.clone());
    reopened.history = loaded.history.clone();
    store.write_all(&reopened).unwrap();
    let again = load(&paths, &opts()).unwrap();
    assert_eq!(again.project, s1.project);
    assert_eq!(again.history.cursor, 1);
    assert!(!again.needs_repair && again.external.is_none());
}

/// The same crash, then a hand edit of `qrec.json`: the file isn't the
/// commit's write anymore, so it is merged as an edit against the head.
#[test]
fn a_crash_after_writing_qrec_json_then_a_hand_edit_is_an_external_edit() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    store.crash_after = Some(Step::Project);
    assert!(commit(&mut store, &s0, &s1, &ops, project).is_err());
    let paths = ProjectPaths::new(tmp.path());
    let mut edited = s1.project.clone();
    edited.name = "edited by hand".into();
    fs::write(paths.project_file(), format::project_bytes(&edited)).unwrap();

    let loaded = load(&paths, &opts()).unwrap();
    // The state starts from the last commit; the newer file is merged by
    // the core as an "External edit" right after opening.
    assert_eq!(loaded.project, s0.project);
    assert!(loaded.history.entries.is_empty());
    assert_eq!(loaded.external, Some(Ok(Box::new(edited.clone()))));
    assert_eq!(
        loaded.accepted_sha.as_deref(),
        Some(format::sha(&format::project_bytes(&edited)).as_str())
    );
}

/// A take finalization that reached `qrec.json` but not the head keeps
/// the facts the file holds (the take `ok`, its media) with the clip it
/// placed. Merged as an edit, the head's facts won (`recording`, 0
/// frames) while the file's clip used the take, so the open reported the
/// file invalid and the placement was lost.
#[test]
fn an_unfinished_finalization_is_finished_with_its_facts() {
    let (tmp, mut store, _) = initialized();
    let take: qrec_core::TakeId = "take-0001".parse().unwrap();
    // The take as `begin_take` saved it, no clip yet.
    let mut recording = state(sample_project());
    recording.project.timeline.clips.clear();
    let t = recording.project.takes.get_mut(&take).unwrap();
    t.status = qrec_core::model::TakeStatus::Recording;
    t.duration_frames = 0;
    t.placed = false;
    store.write_all(&recording).unwrap();
    // Finalization: facts first (persist), then the clip ("Record take 1").
    let mut facts = recording.clone();
    facts
        .project
        .takes
        .insert(take.clone(), sample_project().takes[&take].clone());
    let (finished, ops, project) = edit(&facts, "Record take 1", |p| {
        p.timeline.clips = sample_project().timeline.clips[..1].to_vec();
    });
    store.crash_after = Some(Step::Project);
    assert!(commit(&mut store, &recording, &finished, &ops, project).is_err());

    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, finished.project);
    assert_eq!(
        loaded.project.takes[&take].status,
        qrec_core::model::TakeStatus::Ok
    );
    assert_eq!(
        loaded.history.entries.last().unwrap().label,
        "Record take 1"
    );
    assert_eq!(loaded.external, None);
    qrec_core::validate::ensure_valid(&loaded.project).unwrap();
}

#[test]
fn a_crash_after_the_head_is_committed() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    store.crash_after = Some(Step::Head);
    assert!(commit(&mut store, &s0, &s1, &ops, project).is_err());
    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!(loaded.history.cursor, 1);
    assert_eq!(loaded.external, None);
}

#[test]
fn a_torn_last_line_is_dropped_and_repaired() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    // A crash in the middle of the next append.
    let (s2, _, _) = edit(&s1, "Trim", trim(200));
    let line = format::entry_line(s2.history.entries.last().unwrap());
    let mut bytes = fs::read(store.paths().history_file()).unwrap();
    bytes.extend_from_slice(&line[..line.len() / 3]);
    fs::write(store.paths().history_file(), &bytes).unwrap();

    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!(loaded.history.entries.len(), 1);
    assert!(loaded.needs_repair);
    assert!(
        loaded.notes.iter().any(|n| n.contains("torn")),
        "{:?}",
        loaded.notes
    );

    let mut store = Store::new(ProjectPaths::new(tmp.path()), true);
    store.adopt(&loaded);
    let reopened = State {
        history: loaded.history.clone(),
        ..state(loaded.project.clone())
    };
    store.write_all(&reopened).unwrap();
    assert_eq!(
        fs::read(store.paths().history_file()).unwrap(),
        format::history_bytes(&s1.history.entries[..])
    );

    // A bad line that isn't the last one makes the project invalid.
    let mut bytes = b"{oops}\n".to_vec();
    bytes.extend_from_slice(&format::history_bytes(&s1.history.entries[..]));
    fs::write(store.paths().history_file(), &bytes).unwrap();
    let err = CoreError::from(load(&ProjectPaths::new(tmp.path()), &opts()).unwrap_err());
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(err.message.contains("line 1"), "{}", err.message);
}

#[test]
fn appending_after_an_undo_rewrites_the_redo_tail_away() {
    let (tmp, mut store, s0) = initialized();
    let mut s = s0;
    for out in [90, 80, 70] {
        let (next, ops, project) = edit(&s, &format!("Trim {out}"), trim(out));
        commit(&mut store, &s, &next, &ops, project).unwrap();
        s = next;
    }
    assert_eq!(history_lines(&store), [1, 2, 3]);
    // Two undos (cursor moves; the history ops are SetCursor).
    let undone = qrec_core::history::ops::undo_with(&s, 2, unchecked).unwrap();
    commit(&mut store, &s, &undone, &[HistoryOp::SetCursor(1)], true).unwrap();
    assert_eq!(history_lines(&store), [1, 2, 3], "undo keeps the redo tail");
    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(
        (loaded.history.cursor, loaded.history.entries.len()),
        (1, 3)
    );
    assert_eq!(loaded.project, undone.project);

    let (next, ops, project) = edit(&undone, "Trim 60", trim(60));
    assert_eq!(next.history.cursor, 4);
    commit(&mut store, &undone, &next, &ops, project).unwrap();
    assert_eq!(history_lines(&store), [1, 4]);
    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.history.entries, next.history.entries);
    assert_eq!(loaded.history.next_seq, 5);
}

#[test]
fn rewrites_store_the_base_with_its_seq_and_survive_a_crash() {
    let (tmp, mut store, s0) = initialized();
    let mut s = s0;
    for out in [90, 80, 70] {
        let (next, ops, project) = edit(&s, &format!("Trim {out}"), trim(out));
        commit(&mut store, &s, &next, &ops, project).unwrap();
        s = next;
    }
    // Fold the two oldest entries into the base (like compaction).
    let (compacted, op) =
        qrec_core::history::ops::compact_to(&s.history, &s.project, 1, 1).unwrap();
    let mut next = s.clone();
    next.history = compacted;
    store.crash_after = Some(Step::History);
    assert!(commit(&mut store, &s, &next, std::slice::from_ref(&op), true).is_err());
    let base: BaseFile =
        serde_json::from_slice(&fs::read(store.paths().history_base()).unwrap()).unwrap();
    assert_eq!(base.base_seq, 2);
    assert_eq!(history_lines(&store), [3]);

    // The head still says base 0, but the base file knows better.
    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.history.base_seq, 2);
    assert_eq!(loaded.history.cursor, 3);
    assert_eq!(loaded.history.entries.len(), 1);
    assert_eq!(loaded.project, s.project);
    assert!(
        loaded.notes.iter().any(|n| n.contains("didn't finish")),
        "{:?}",
        loaded.notes
    );
    assert!(loaded.needs_repair, "the commit record is written again");

    // Undoing the kept entry lands exactly on the stored base.
    let reopened = State {
        history: loaded.history.clone(),
        ..state(loaded.project.clone())
    };
    let undone = qrec_core::history::ops::undo_with(&reopened, 1, unchecked).unwrap();
    assert_eq!(undone.project, base.project);
    assert_eq!(undone.history.cursor, 2);
}

/// An unreadable `history.base.json` (or a missing one, with entries) is
/// derived again from the project at the cursor and written with the
/// repairs, so the history can still rebuild the project (A§7.7). It used
/// to be ignored, never written again (`has_base` saw the file), and a
/// recovery said there was no base.
#[test]
fn a_lost_base_is_derived_from_the_cursor_and_written() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let (s2, ops, project) = edit(&s1, "Trim", trim(250));
    commit(&mut store, &s1, &s2, &ops, project).unwrap();
    let paths = ProjectPaths::new(tmp.path());
    let original = fs::read(paths.history_base()).unwrap();
    let base_project = s0.project.clone();

    for (lost, what) in [(Some(&b"garbage\n"[..]), "unreadable"), (None, "missing")] {
        match lost {
            Some(bytes) => fs::write(paths.history_base(), bytes).unwrap(),
            None => fs::remove_file(paths.history_base()).unwrap(),
        }
        for opts in [opts(), recovery()] {
            let loaded = load(&paths, &opts).unwrap();
            assert!(!loaded.has_base, "{what}");
            assert_eq!(
                loaded.base_to_write,
                Some((0, base_project.clone())),
                "{what}"
            );
            assert!(loaded.needs_repair, "{what}");
            assert_eq!(loaded.history.entries.len(), 2, "{what}");
            let note = format!("rebuilt the {what} .qrec/history.base.json from the history");
            assert!(
                loaded.notes.iter().any(|n| n.starts_with(&note)),
                "{what}: {:?}",
                loaded.notes
            );
        }
        // The open's repair writes the same base again.
        let loaded = load(&paths, &opts()).unwrap();
        let mut store = Store::new(paths.clone(), true);
        store.adopt(&loaded);
        let reopened = State {
            history: loaded.history.clone(),
            ..state(loaded.project.clone())
        };
        store.write_all(&reopened).unwrap();
        assert_eq!(fs::read(paths.history_base()).unwrap(), original, "{what}");
        let again = load(&paths, &opts()).unwrap();
        assert!(again.has_base && !again.needs_repair, "{what}");
        assert!(again.notes.is_empty(), "{what}: {:?}", again.notes);
    }

    // The history rebuilds the project from it.
    fs::remove_file(paths.history_head()).unwrap();
    fs::remove_file(paths.project_file()).unwrap();
    let rebuilt = load(&paths, &recovery()).unwrap();
    assert_eq!(rebuilt.history.cursor, 2);
    assert_eq!(rebuilt.project, s2.project);

    // With nothing else to recover from, an unreadable base is named so.
    fs::write(paths.history_base(), b"garbage\n").unwrap();
    let err = CoreError::from(load(&paths, &recovery()).unwrap_err());
    assert!(
        err.message
            .ends_with("and .qrec/history.base.json is unreadable"),
        "{}",
        err.message
    );
}

#[test]
fn a_cursor_lost_from_the_history_starts_it_over() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    fs::write(store.paths().history_file(), b"").unwrap();
    let loaded = load(&ProjectPaths::new(tmp.path()), &opts()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!((loaded.history.cursor, loaded.history.base_seq), (1, 1));
    assert!(loaded.history.entries.is_empty());
    assert_eq!(loaded.base_to_write, Some((1, s1.project.clone())));
    assert!(loaded.needs_repair);
}

#[test]
fn projects_without_a_commit_record_load_and_get_one() {
    let tmp = tempfile::tempdir().unwrap();
    let project = sample_project();
    fs::write(
        tmp.path().join("qrec.json"),
        format::project_bytes(&project),
    )
    .unwrap();
    let paths = ProjectPaths::new(tmp.path());
    let loaded = load(&paths, &opts()).unwrap();
    assert_eq!(loaded.project, project);
    assert_eq!(
        (
            loaded.history.cursor,
            loaded.history.base_seq,
            loaded.history.next_seq
        ),
        (0, 0, 1)
    );
    assert!(loaded.needs_repair);
    assert_eq!(loaded.base_to_write, Some((0, project.clone())));

    fs::create_dir(paths.qrec_dir()).unwrap();
    let mut store = Store::new(paths.clone(), true);
    store.adopt(&loaded);
    assert!(store.is_dirty());
    let s0 = State {
        history: loaded.history.clone(),
        ..state(loaded.project.clone())
    };
    store.write_all(&s0).unwrap();
    let again = load(&paths, &opts()).unwrap();
    assert!(!again.needs_repair, "{:?}", again.notes);

    // History lines without a head: the cursor is the entry matching the file.
    let (s1, ops, flag) = edit(&s0, "Trim 90", trim(90));
    commit(&mut store, &s0, &s1, &ops, flag).unwrap();
    let (s2, ops, flag) = edit(&s1, "Trim 80", trim(80));
    commit(&mut store, &s1, &s2, &ops, flag).unwrap();
    fs::remove_file(paths.history_head()).unwrap();
    fs::write(paths.project_file(), format::project_bytes(&s1.project)).unwrap();
    let loaded = load(&paths, &opts()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!((loaded.history.cursor, loaded.history.next_seq), (1, 3));
    assert_eq!(loaded.history.entries.len(), 2, "the redo tail survives");
}

#[test]
fn invalid_or_missing_files_fail_with_the_restore_hint() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let paths = ProjectPaths::new(tmp.path());

    fs::write(paths.project_file(), b"{ not json").unwrap();
    let err = CoreError::from(load(&paths, &opts()).unwrap_err());
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(
        err.message.starts_with("qrec.json is not valid JSON"),
        "{}",
        err.message
    );
    assert_eq!(
        err.hint.as_deref(),
        Some("fix it, or rebuild it from the history with `qrec history restore 1 --yes`")
    );
    // A file that parses but fails the check.
    fs::write(paths.project_file(), format::project_bytes(&s1.project)).unwrap();
    let refuse: Check = |_| {
        Err(CoreError::invalid(
            "invalid project: clip c002 is too long (/timeline/clips/0)",
        ))
    };
    let err = CoreError::from(
        load(
            &paths,
            &LoadOpts {
                recovery: false,
                check: refuse,
            },
        )
        .unwrap_err(),
    );
    assert!(
        err.message
            .starts_with("qrec.json is invalid: invalid project"),
        "{}",
        err.message
    );

    fs::remove_file(paths.project_file()).unwrap();
    let err = CoreError::from(load(&paths, &opts()).unwrap_err());
    assert_eq!(err.message, "qrec.json is missing");
    assert!(err.hint.unwrap().contains("qrec history restore 1 --yes"));
}

#[test]
fn recovery_mode_uses_the_head_and_never_an_unusable_file() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let paths = ProjectPaths::new(tmp.path());
    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!(loaded.disk_invalid, None, "the file matches the head");
    assert_eq!(loaded.external, None);

    fs::write(paths.project_file(), b"garbage").unwrap();
    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!(loaded.project, s1.project);
    assert_eq!(loaded.history.cursor, 1);
    assert_eq!(loaded.external, None);
    assert!(
        loaded
            .disk_invalid
            .as_deref()
            .unwrap()
            .starts_with("qrec.json is not valid JSON"),
        "{:?}",
        loaded.disk_invalid
    );
    assert_eq!(
        loaded.accepted_sha.as_deref(),
        Some(format::sha(b"garbage").as_str())
    );

    fs::remove_file(paths.project_file()).unwrap();
    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!(loaded.disk_invalid.as_deref(), Some("qrec.json is missing"));
}

#[test]
fn recovery_mode_merges_a_valid_hand_edit_like_any_open() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let paths = ProjectPaths::new(tmp.path());

    // Same project, other formatting: nothing to merge.
    let compact = serde_json::to_vec(&s1.project).unwrap();
    fs::write(paths.project_file(), &compact).unwrap();
    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!((loaded.external, loaded.disk_invalid), (None, None));

    // A valid edit is an external edit for the session to merge (SPEC §8.1),
    // not a broken file to copy aside and replace.
    let mut edited = s1.project.clone();
    edited.name = "my hand edit".into();
    fs::write(paths.project_file(), format::project_bytes(&edited)).unwrap();
    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!(loaded.disk_invalid, None);
    assert_eq!(loaded.external, Some(Ok(Box::new(edited.clone()))));
    assert_eq!(loaded.project, s1.project, "the head, to merge into");
    assert_eq!(loaded, load(&paths, &opts()).unwrap(), "like a normal open");
}

#[test]
fn recovery_rebuilds_from_the_base_and_re_adds_takes() {
    let (tmp, mut store, s0) = initialized();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    let (s2, ops, project) = edit(&s1, "Trim", trim(250));
    commit(&mut store, &s1, &s2, &ops, project).unwrap();
    let paths = ProjectPaths::new(tmp.path());
    // A take recorded after the base, still on disk.
    let take_dir = paths.takes_dir().join("take-0002");
    fs::create_dir_all(&take_dir).unwrap();
    for name in ["L1-screen-FAKE-1.mkv", "A1-mic.mka", "recorder.log"] {
        fs::write(take_dir.join(name), b"x").unwrap();
    }
    fs::remove_file(paths.history_head()).unwrap();
    fs::write(paths.project_file(), b"{").unwrap();

    let err = CoreError::from(load(&paths, &opts()).unwrap_err());
    assert!(
        err.hint.unwrap().contains("qrec history restore 2 --yes"),
        "no head, but the history can rebuild it"
    );

    let loaded = load(&paths, &recovery()).unwrap();
    assert_eq!(loaded.history.cursor, 2);
    assert_eq!(loaded.project.timeline, s2.project.timeline);
    assert_eq!(loaded.readded, vec!["take-0002".parse().unwrap()]);
    let take = &loaded.project.takes[&"take-0002".parse().unwrap()];
    assert_eq!(take.status, qrec_core::model::TakeStatus::Recording);
    let files: Vec<(&str, &str)> = take
        .streams
        .iter()
        .map(|s| (s.id.as_str(), s.file.as_str()))
        .collect();
    assert_eq!(
        files,
        [
            ("A1", "takes/take-0002/A1-mic.mka"),
            ("L1", "takes/take-0002/L1-screen-FAKE-1.mkv")
        ]
    );
    assert_eq!(loaded.project.next_take_number, 3);
    assert!(
        loaded
            .disk_invalid
            .as_deref()
            .unwrap()
            .starts_with("qrec.json is not valid JSON"),
        "{:?}",
        loaded.disk_invalid
    );
    assert!(loaded.needs_repair);
    assert!(!take.placed, "no entry placed it: it goes to the bin");
    assert_eq!(take.scene, loaded.project.scene, "the latest scene");

    // Nothing at all to recover from.
    fs::remove_file(paths.history_base()).unwrap();
    let err = CoreError::from(load(&paths, &recovery()).unwrap_err());
    assert!(
        err.message.starts_with("nothing to recover from"),
        "{}",
        err.message
    );
}

/// A project whose base predates its only take, like every project
/// recorded after `qrec init`: e1 adds the screen layer, the take enters
/// the pool as a fact (no entry), e2 places its clip ("Record take 1"),
/// e3 edits the take's own scene.
fn recorded_after_the_base() -> (tempfile::TempDir, Store, Vec<State>) {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".qrec")).unwrap();
    let mut store = Store::new(ProjectPaths::new(tmp.path()), true);
    let full = sample_project();
    let mut empty = full.clone();
    empty.takes.clear();
    empty.timeline.clips.clear();
    empty.scene.layers.clear();
    empty.next_take_number = 1;
    let s0 = state(empty);
    store.write_all(&s0).unwrap();
    let (s1, ops, project) = edit(&s0, "Add layer L1", |p| {
        p.scene.layers = full.scene.layers.clone()
    });
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    // begin_take: the take is a fact, saved without an entry.
    let mut taken = s1.clone();
    let mut take = full.takes.values().next().unwrap().clone();
    take.scene = s1.project.scene.clone();
    taken
        .project
        .takes
        .insert("take-0001".parse().unwrap(), take);
    taken.project.next_take_number = 2;
    commit(&mut store, &s1, &taken, &[], true).unwrap();
    let (s2, ops, project) = edit(&taken, "Record take 1", |p| {
        p.timeline.clips = full.timeline.clips[..1].to_vec()
    });
    commit(&mut store, &taken, &s2, &ops, project).unwrap();
    let (s3, ops, project) = edit(&s2, "Fade take 1's screen", |p| {
        let take = p.takes.values_mut().next().unwrap();
        take.scene.layers[0].opacity = 0.5;
    });
    commit(&mut store, &s2, &s3, &ops, project).unwrap();
    (tmp, store, vec![s0, s1, s2, s3])
}

#[test]
fn a_rebuilt_take_joins_the_pool_where_its_history_needs_it() {
    let (tmp, _store, states) = recorded_after_the_base();
    let paths = ProjectPaths::new(tmp.path());
    let take_id: qrec_core::TakeId = "take-0001".parse().unwrap();
    let base: BaseFile = serde_json::from_slice(&fs::read(paths.history_base()).unwrap()).unwrap();
    assert!(base.project.takes.is_empty(), "the base predates the take");
    let dir = paths.take_dir(&take_id);
    fs::create_dir_all(&dir).unwrap();
    for name in ["L1-screen-FAKE-1.mkv", "A1-mic.mka"] {
        fs::write(dir.join(name), b"x").unwrap();
    }
    fs::remove_file(paths.history_head()).unwrap();
    fs::write(paths.project_file(), b"garbage").unwrap();

    let loaded = load(&paths, &recovery()).unwrap();
    let last = &states[3].project;
    assert_eq!(loaded.history.cursor, 3, "{:?}", loaded.notes);
    assert_eq!(loaded.project.timeline, last.timeline);
    assert_eq!(loaded.readded, vec![take_id.clone()]);
    let take = &loaded.project.takes[&take_id];
    // The scene the project had when the take was recorded, so e3 (a patch
    // of that scene, guarded by tests of its old values) still applies.
    assert_eq!(take.scene, last.takes[&take_id].scene);
    assert_eq!(take.scene.layers[0].opacity, 0.5);
    assert!(take.placed, "e2 placed it: discard-last can find it");
    assert_eq!(take.status, qrec_core::model::TakeStatus::Recording);
    assert_eq!(take.duration_frames, 0, "the Recover probe measures it");
    assert_eq!(loaded.project.next_take_number, 2);
}

/// SPEC §5.3.1: take numbers are never reused. A project rebuilt from
/// `.qrec/` alone, whose take folders are gone too, starts numbering after
/// every take its history names: the base predates them, and a new take
/// numbered like an old one would get the old entries' clips on redo.
#[test]
fn a_rebuilt_project_never_reuses_a_take_number_its_history_names() {
    let (tmp, _store, _states) = recorded_after_the_base();
    let paths = ProjectPaths::new(tmp.path());
    let base: BaseFile = serde_json::from_slice(&fs::read(paths.history_base()).unwrap()).unwrap();
    assert_eq!(base.project.next_take_number, 1, "the base predates take 1");
    assert!(!paths.takes_dir().exists(), "no take folder to count");
    fs::remove_file(paths.history_head()).unwrap();
    fs::remove_file(paths.project_file()).unwrap();

    let loaded = load(&paths, &recovery()).unwrap();
    assert!(loaded.readded.is_empty());
    assert_eq!(
        loaded.project.next_take_number, 2,
        "take 1 is named by the history: {:?}",
        loaded.notes
    );
    let named = loaded
        .history
        .entries
        .iter()
        .any(|e| e.label == "Record take 1");
    assert!(named, "{:?}", loaded.history.entries);
}

#[test]
fn an_outcome_that_saves_nothing_never_writes_even_when_dirty() {
    let (tmp, mut store, s0) = initialized();
    let paths = ProjectPaths::new(tmp.path());
    fs::write(paths.project_file(), b"{ a broken hand edit").unwrap();
    store.mark_dirty();
    let mut next = s0.clone();
    next.runtime.capabilities.cores = 3;
    commit(&mut store, &s0, &next, &[], false).unwrap();
    assert_eq!(
        fs::read(paths.project_file()).unwrap(),
        b"{ a broken hand edit",
        "a capabilities event, a clock tick… write nothing"
    );
    assert!(
        store.is_dirty(),
        "the next change that saves rewrites everything"
    );
    // The next change that saves does.
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    assert!(!store.is_dirty());
    assert_eq!(
        fs::read(paths.project_file()).unwrap(),
        format::project_bytes(&s1.project)
    );
}

#[test]
fn the_cursor_file_follows_every_commit() {
    let (tmp, mut store, s0) = initialized();
    let paths = ProjectPaths::new(tmp.path());
    let cursor = || fs::read_to_string(paths.history_cursor()).unwrap();
    assert_eq!(cursor(), "0\n", "SPEC §8.1: the current seq");
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    assert_eq!(cursor(), "1\n");
    let undone = qrec_core::history::ops::undo_with(&s1, 1, unchecked).unwrap();
    commit(&mut store, &s1, &undone, &[HistoryOp::SetCursor(0)], true).unwrap();
    assert_eq!(cursor(), "0\n");
    // A copy for readers: stale or missing, it is put right, never read.
    fs::write(paths.history_cursor(), b"7\n").unwrap();
    assert_eq!(load(&paths, &opts()).unwrap().history.cursor, 0);
    store.sync_cursor_file(0);
    assert_eq!(cursor(), "0\n");
    fs::remove_file(paths.history_cursor()).unwrap();
    store.sync_cursor_file(0);
    assert_eq!(cursor(), "0\n");
    let reader = Store::new(paths.clone(), false);
    fs::remove_file(paths.history_cursor()).unwrap();
    reader.sync_cursor_file(0);
    assert!(!paths.history_cursor().exists(), "readers write nothing");
}

#[test]
fn disk_changes_are_detected_by_hash() {
    let (_tmp, mut store, s0) = initialized();
    assert_eq!(store.disk_changed(unchecked), None, "our own write");

    let mut edited = s0.project.clone();
    edited.name = "renamed by hand".into();
    fs::write(store.paths().project_file(), format::project_bytes(&edited)).unwrap();
    let file = store.disk_changed(unchecked).expect("an external edit");
    assert_eq!(file.project, Ok(Box::new(edited)));
    store.accept_disk(&file);
    assert_eq!(store.disk_changed(unchecked), None, "seen once");

    fs::write(store.paths().project_file(), b"{\"version\":1}").unwrap();
    let file = store.disk_changed(unchecked).unwrap();
    assert!(
        file.project
            .as_ref()
            .unwrap_err()
            .contains("doesn't match the project schema")
    );
    store.accept_disk(&file);

    fs::remove_file(store.paths().project_file()).unwrap();
    let file = store.disk_changed(unchecked).unwrap();
    assert_eq!(
        (file.sha, file.project),
        (None, Err("qrec.json was deleted".to_owned()))
    );
}

#[test]
fn bad_files_are_moved_or_copied_aside_without_overwriting() {
    let (_tmp, mut store, _) = initialized();
    let paths = store.paths().clone();
    fs::write(paths.project_file(), b"bad 1").unwrap();
    let copy = store
        .copy_disk_file_aside("20261014-120000")
        .unwrap()
        .unwrap();
    assert_eq!(copy, paths.corrupt_copy("20261014-120000"));
    let copy2 = store
        .copy_disk_file_aside("20261014-120000")
        .unwrap()
        .unwrap();
    assert_eq!(
        copy2.file_name().unwrap(),
        "qrec.corrupt-20261014-120000-2.json"
    );
    assert!(paths.project_file().exists(), "copying leaves the file");

    let moved = store.reject_disk_file("20261014-120000").unwrap().unwrap();
    assert_eq!(moved, paths.rejected_file("20261014-120000"));
    assert!(!paths.project_file().exists());
    assert_eq!(store.last_sha(), None);
    fs::write(paths.project_file(), b"bad 2").unwrap();
    let moved2 = store.reject_disk_file("20261014-120000").unwrap().unwrap();
    assert_eq!(moved2.file_name().unwrap(), "qrec-20261014-120000-2.json");
    assert_eq!(fs::read(&moved).unwrap(), b"bad 1");
    assert_eq!(fs::read(&moved2).unwrap(), b"bad 2");
    assert_eq!(
        store.reject_disk_file("x").unwrap(),
        None,
        "nothing left to move"
    );
}

#[test]
fn a_migrated_file_is_backed_up_before_the_first_save() {
    let (_tmp, mut store, s0) = initialized();
    store.pending_backup = Some((0, b"old version".to_vec()));
    store.write_all(&s0).unwrap();
    store.pending_backup = Some((0, b"old version again".to_vec()));
    store.write_all(&s0).unwrap();
    let paths = store.paths();
    assert_eq!(fs::read(paths.migration_backup(0)).unwrap(), b"old version");
    assert_eq!(
        fs::read(paths.qrec_dir().join("qrec.v0-2.json.bak")).unwrap(),
        b"old version again"
    );
}

#[test]
fn ui_writes_are_coalesced() {
    let (_tmp, mut store, s0) = initialized();
    let mut ui = s0.ui.clone();
    let t0 = Instant::now();
    ui.playhead = 5;
    store.want_ui_write(&ui, t0).unwrap();
    let read = |store: &Store| -> UiState {
        serde_json::from_slice(&fs::read(store.paths().ui_file()).unwrap()).unwrap()
    };
    assert_eq!(read(&store).playhead, 5);
    ui.playhead = 9;
    store
        .want_ui_write(&ui, t0 + Duration::from_millis(100))
        .unwrap();
    assert_eq!(read(&store).playhead, 5, "coalesced");
    assert_eq!(store.ui_deadline(), Some(t0 + UI_WRITE_INTERVAL));
    store
        .flush_ui(&ui, t0 + Duration::from_millis(200), false)
        .unwrap();
    assert_eq!(read(&store).playhead, 5, "not due yet");
    store.flush_ui(&ui, t0 + UI_WRITE_INTERVAL, false).unwrap();
    assert_eq!(read(&store).playhead, 9);
    assert_eq!(store.ui_deadline(), None);
}

#[test]
fn take_dirs_are_never_reused() {
    let (_tmp, store, _) = initialized();
    let path = store.create_take_dir("takes/take-0002").unwrap();
    assert!(path.is_dir());
    let err = store.create_take_dir("takes/take-0002").unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    assert!(store.create_take_dir("exports/x").is_err());
    assert!(store.create_take_dir("takes/../x").is_err());
    assert_eq!(take_dirs(store.paths()), vec!["take-0002".parse().unwrap()]);
}

#[test]
fn read_only_stores_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::new(ProjectPaths::new(tmp.path()), false);
    let s0 = state(sample_project());
    store.write_all(&s0).unwrap();
    let (s1, ops, project) = edit(&s0, "Remove c001", remove_first_clip);
    commit(&mut store, &s0, &s1, &ops, project).unwrap();
    store.want_ui_write(&s1.ui, Instant::now()).unwrap();
    assert!(store.create_take_dir("takes/take-0002").is_err());
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}
