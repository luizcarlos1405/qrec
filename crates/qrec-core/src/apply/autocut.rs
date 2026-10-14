//! The `Autocut` command (A§5.7). Owner: autocut. (`AutocutSettings`, which
//! edits `project.autocut`, belongs to the scene owner: `apply::project`.)
//!
//! `Autocut` snapshots its parameters and targets (`AutocutTarget`: the
//! clip as a `ClipRef`, the track, the `SilenceKey` of its audio file from
//! `ctx.facts.files`) into a `JobKind::Autocut` job and emits one
//! `DetectSilence` per key missing from `runtime.silence`. The job completes
//! in `on_event::autocut` — or right here when every key is already known.
//! Clips that can't be analyzed (no such track, missing media, no file) are
//! skipped with a reason; until the shared job type has a field for them,
//! they ride in the job's `cmd` (see `on_event::autocut::with_skipped`).
//!
//! `start_auto_run` is the same flow for `autocut.autoRunAfterTake`, called
//! by the record owner right after a take is placed.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::apply::Outcome;
use crate::autocut::{analysis_source, db_label};
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{ClipId, ReqId, TrackId};
use crate::media_argv::silence_argv;
use crate::model::{AutocutSettings, Clip, Project, Take};
use crate::on_event::autocut::{self as events, Snapshot};
use crate::protocol::command::{AutocutOverrides, ClipScope, Command};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::reply::AutocutSkipView;
use crate::state::{AutocutParams, AutocutTarget, ClipRef, Job, JobKind, SilenceKey, State};

pub use crate::on_event::autocut::apply_ready_autocuts;

/// Thresholds outside −90…0 dB are refused (A§2.3).
pub const THRESHOLD_DB_RANGE: std::ops::RangeInclusive<f64> = -90.0..=0.0;
/// `minSilenceMs`, `paddingMs` and `minKeepMs` are at most a minute (A§2.3).
pub const MAX_MS: u32 = 60_000;

/// `qrec autocut [--clips …|--all] [--dry-run] …`.
pub fn autocut(
    state: &State,
    ctx: &Ctx,
    scope: ClipScope,
    overrides: AutocutOverrides,
    dry_run: bool,
) -> Result<Outcome, CoreError> {
    let params = effective_params(&state.project.autocut, &overrides)?;
    // A job that finished analyzing while a recording ran and was never
    // applied (the recording ended without placing a take) goes first.
    let (base, mut effects) = if dry_run {
        (state.clone(), Vec::new())
    } else {
        events::flush_ready(state, ctx)
    };
    let clips = clips_in_scope(&base.project, &scope)?;
    if let Some(track) = &overrides.track {
        let takes = clips.iter().filter_map(|c| base.project.take(&c.take));
        ensure_track_known(&base.project, track, takes)?;
    }
    let cmd = Command::Autocut {
        scope,
        overrides: overrides.clone(),
        dry_run,
    }
    .to_json();
    let request = Request {
        cmd,
        clips,
        params,
        track: overrides.track,
        dry_run,
        req: ctx.req.clone(),
    };
    Ok(match start(&base, ctx, request) {
        Started::Done(done) => {
            effects.extend(done.effects);
            Outcome::new(done.state, effects, Some(done.reply))
        }
        Started::Pending {
            state,
            effects: more,
        } => {
            effects.extend(more);
            Outcome::new(state, effects, None)
        }
    })
}

/// Starts autocut on `clip` for `autocut.autoRunAfterTake` (SPEC §5.3 step
/// 6): the record owner calls it right after placing a take. A no-op unless
/// the setting is on. It becomes its own history entry (cmd
/// `{"event":"autoRunAfterTake",…}`) when the analysis completes; there is
/// no requester, so the outcome is told with a toast.
pub fn start_auto_run(state: &State, ctx: &Ctx, clip: &ClipId) -> Outcome {
    let settings = &state.project.autocut;
    if !settings.auto_run_after_take {
        return Outcome::unchanged(state);
    }
    let Some(target) = state.project.clip(clip).cloned() else {
        return Outcome::new(
            state.clone(),
            vec![log(
                LogLevel::Debug,
                format!("autocut after take: clip {clip} is not on the timeline"),
            )],
            None,
        );
    };
    let params = match effective_params(settings, &AutocutOverrides::default()) {
        Ok(params) => params,
        Err(err) => {
            let toast = events::failure_toast(&err);
            return Outcome::new(state.clone(), vec![Effect::Notify(toast)], None);
        }
    };
    let request = Request {
        cmd: json!({"event": "autoRunAfterTake", "take": target.take, "clip": target.id}),
        clips: vec![target],
        params,
        track: None,
        dry_run: false,
        req: None,
    };
    match start(state, ctx, request) {
        Started::Done(done) => {
            let (state, effects) = done.deliver(None);
            Outcome::new(state, effects, None)
        }
        Started::Pending { state, effects } => Outcome::new(state, effects, None),
    }
}

// ------------------------------------------------------------- the job

/// What a run analyzes, before the targets are resolved.
struct Request {
    /// The history entry's `cmd`.
    cmd: Value,
    /// In timeline order.
    clips: Vec<Clip>,
    params: AutocutParams,
    /// The override track, if any.
    track: Option<TrackId>,
    dry_run: bool,
    req: Option<ReqId>,
}

enum Started {
    /// Every key was known: the run completed (or nothing was analyzable).
    Done(events::Finished),
    /// A job waits for `DetectSilence` results.
    Pending { state: State, effects: Vec<Effect> },
}

/// Resolves targets, then completes at once or creates the job.
fn start(state: &State, ctx: &Ctx, request: Request) -> Started {
    let project = &state.project;
    let params = request.params;
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    // Keys in first-use order, with the file each one analyzes.
    let mut files: Vec<(SilenceKey, String)> = Vec::new();
    for clip in &request.clips {
        let source = project
            .take(&clip.take)
            .ok_or_else(|| format!("take {} doesn't exist", clip.take))
            .and_then(|take| analysis_source(project, &clip.take, take, request.track.as_ref()));
        let (track, stream) = match source {
            Ok(found) => found,
            Err(reason) => {
                skipped.push(AutocutSkipView {
                    clip: clip.id.clone(),
                    reason,
                });
                continue;
            }
        };
        let Some(fact) = ctx.facts.files.get(&stream.file) else {
            skipped.push(AutocutSkipView {
                clip: clip.id.clone(),
                reason: format!("audio file {} not found", stream.file),
            });
            continue;
        };
        let key = SilenceKey::compute(
            &stream.file,
            fact.bytes,
            fact.mtime_ns,
            params.threshold_db,
            params.min_silence_ms,
        );
        if !files.iter().any(|(k, _)| *k == key) {
            files.push((key.clone(), stream.file.clone()));
        }
        targets.push(AutocutTarget {
            clip: ClipRef::of(clip),
            track: track.id.clone(),
            key,
        });
    }

    let missing: Vec<&(SilenceKey, String)> = files
        .iter()
        .filter(|(key, _)| !state.runtime.silence.contains_key(key))
        .collect();
    if missing.is_empty() {
        let snapshot = Snapshot {
            cmd: request.cmd,
            targets,
            params,
            dry_run: request.dry_run,
            skipped,
        };
        return Started::Done(events::finish(state, ctx, snapshot));
    }

    let mut next = state.clone();
    let job = next.runtime.alloc_job_id();
    let mut effects: Vec<Effect> = missing
        .iter()
        .map(|(key, file)| Effect::DetectSilence {
            job,
            key: key.clone(),
            argv: silence_argv(file, params.threshold_db, params.min_silence_ms),
        })
        .collect();
    let pending: BTreeSet<SilenceKey> = missing.iter().map(|(key, _)| key.clone()).collect();
    let kind = JobKind::Autocut {
        cmd: events::with_skipped(request.cmd, &skipped),
        targets,
        params,
        dry_run: request.dry_run,
        pending,
        ready: false,
    };
    let entry = Job {
        id: job,
        kind,
        req: request.req,
        started: ctx.now,
    };
    if let Some(progress) = events::progress_of(&entry, ctx) {
        effects.push(progress);
    }
    next.runtime.jobs.insert(job, entry);
    Started::Pending {
        state: next,
        effects,
    }
}

// ------------------------------------------------------------ helpers

/// The project settings with the command's overrides, checked against A§2.3.
fn effective_params(
    settings: &AutocutSettings,
    overrides: &AutocutOverrides,
) -> Result<AutocutParams, CoreError> {
    let base = params_of(settings);
    let params = AutocutParams {
        threshold_db: overrides.threshold_db.unwrap_or(base.threshold_db),
        min_silence_ms: overrides.min_silence_ms.unwrap_or(base.min_silence_ms),
        padding_ms: overrides.padding_ms.unwrap_or(base.padding_ms),
        min_keep_ms: overrides.min_keep_ms.unwrap_or(base.min_keep_ms),
    };
    check_params(&params)?;
    Ok(params)
}

fn params_of(settings: &AutocutSettings) -> AutocutParams {
    AutocutParams {
        threshold_db: settings.threshold_db,
        min_silence_ms: settings.min_silence_ms,
        padding_ms: settings.padding_ms,
        min_keep_ms: settings.min_keep_ms,
    }
}

fn check_params(params: &AutocutParams) -> Result<(), CoreError> {
    if !THRESHOLD_DB_RANGE.contains(&params.threshold_db) {
        return Err(CoreError::usage(format!(
            "the autocut threshold must be between \u{2212}90 and 0 dB (got {})",
            db_label(params.threshold_db)
        ))
        .with_hint("speech usually sits well above \u{2212}35 dB; try --threshold-db -35"));
    }
    for (what, ms) in [
        ("minimum silence", params.min_silence_ms),
        ("padding", params.padding_ms),
        ("minimum kept fragment", params.min_keep_ms),
    ] {
        if ms > MAX_MS {
            return Err(CoreError::usage(format!(
                "the autocut {what} must be at most {MAX_MS} ms (got {ms} ms)"
            )));
        }
    }
    Ok(())
}

/// The clips `scope` names, in timeline order and without duplicates.
fn clips_in_scope(project: &Project, scope: &ClipScope) -> Result<Vec<Clip>, CoreError> {
    match scope {
        ClipScope::All => {
            if project.timeline.clips.is_empty() {
                return Err(
                    CoreError::invalid("nothing to autocut: the timeline is empty")
                        .with_hint("record a take first (qrec record start)"),
                );
            }
            Ok(project.timeline.clips.clone())
        }
        ClipScope::Clips(ids) => {
            if ids.is_empty() {
                return Err(CoreError::usage("no clips to autocut")
                    .with_hint("pass clip ids, or --all for the whole timeline"));
            }
            if let Some(unknown) = ids.iter().find(|id| project.clip(id).is_none()) {
                return Err(
                    CoreError::not_found(format!("no clip {unknown} on the timeline"))
                        .with_hint("list the clips with qrec clip list"),
                );
            }
            let wanted: BTreeSet<&ClipId> = ids.iter().collect();
            Ok(project
                .timeline
                .clips
                .iter()
                .filter(|c| wanted.contains(&c.id))
                .cloned()
                .collect())
        }
    }
}

/// A track id must exist somewhere: in the project scene or in the scene of
/// one of `takes`. Takes that lack it are skipped later, with a reason.
fn ensure_track_known<'a>(
    project: &Project,
    track: &TrackId,
    mut takes: impl Iterator<Item = &'a Take>,
) -> Result<(), CoreError> {
    let known =
        project.scene.track(track).is_some() || takes.any(|take| take.scene.track(track).is_some());
    if known {
        Ok(())
    } else {
        Err(CoreError::not_found(format!("no audio track {track}"))
            .with_hint("use the id of an audio track in the scene, such as A1"))
    }
}

fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log { level, message }
}

/// Fixtures shared by the autocut flow tests (`apply` and `on_event`).
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use crate::apply::Outcome;
    use crate::ctx::{Ctx, FileFact};
    use crate::model::{Project, StreamKind};
    use crate::protocol::effect::{Effect, HistoryOp};
    use crate::protocol::event::Interval;
    use crate::state::{AutocutParams, SilenceKey, State};
    use crate::testkit::{self, ProjectBuilder};
    use crate::time::NS_PER_MS;

    pub const NOW: &str = "2026-10-14T12:30:00Z";
    pub const MTIME_NS: i64 = 1_760_443_200_000_000_000;
    pub const MIC1: &str = "takes/take-0001/A1-mic.mka";
    pub const MIC2: &str = "takes/take-0002/A1-mic.mka";
    pub const SYSTEM1: &str = "takes/take-0001/A2-system.mka";

    /// Two takes with a mic and system audio, one clip each: take 1 is
    /// 10 s (300 frames at 30 fps), take 2 is 5 s.
    pub fn project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 300)
            .take("take-0002", 150)
            .clip("c001", "take-0001", 0, 300)
            .clip("c002", "take-0002", 0, 150)
            .build()
    }

    pub fn ms(start: i64, end: i64) -> Interval {
        Interval {
            start_ns: start * NS_PER_MS,
            end_ns: end * NS_PER_MS,
        }
    }

    /// What ffmpeg finds in each file (file time).
    pub fn silences(file: &str) -> Vec<Interval> {
        match file {
            MIC1 => vec![ms(0, 800), ms(3000, 4500), ms(8000, 10_000)],
            MIC2 => vec![ms(1000, 1800)],
            SYSTEM1 => vec![ms(5000, 9000)],
            _ => Vec::new(),
        }
    }

    pub fn params() -> AutocutParams {
        AutocutParams {
            threshold_db: -35.0,
            min_silence_ms: 400,
            padding_ms: 120,
            min_keep_ms: 200,
        }
    }

    /// A context whose facts know every stream file of `project`.
    pub fn ctx(project: &Project) -> Ctx {
        let mut ctx = testkit::ctx(NOW, 7);
        let mut files = BTreeMap::new();
        for (i, stream) in project
            .takes
            .values()
            .flat_map(|t| t.streams.iter())
            .enumerate()
        {
            let fact = FileFact {
                path: stream.file.clone(),
                bytes: 10_000 + i as u64,
                mtime_ns: MTIME_NS,
            };
            files.insert(stream.file.clone(), fact);
        }
        ctx.facts.files = files;
        ctx
    }

    /// The silence key of `file` under `ctx`'s facts.
    pub fn key(ctx: &Ctx, file: &str, params: &AutocutParams) -> SilenceKey {
        let fact = &ctx.facts.files[file];
        SilenceKey::compute(
            file,
            fact.bytes,
            fact.mtime_ns,
            params.threshold_db,
            params.min_silence_ms,
        )
    }

    /// `state` with the analysis of every audio file already cached.
    pub fn cached(mut state: State, ctx: &Ctx, params: &AutocutParams) -> State {
        let files: Vec<String> = state
            .project
            .takes
            .values()
            .flat_map(|t| t.streams.iter())
            .filter(|s| s.kind == StreamKind::Audio)
            .map(|s| s.file.clone())
            .collect();
        for file in files {
            let key = key(ctx, &file, params);
            state.runtime.silence.insert(key, silences(&file));
        }
        state
    }

    /// An outcome without the parts other owners decide (patches, hashes).
    pub fn digest(outcome: &Outcome) -> Value {
        json!({
            "effects": outcome.effects.iter().map(effect_digest).collect::<Vec<_>>(),
            "reply": outcome.reply,
            "timeline": outcome.state.project.timeline.clips,
            "jobs": outcome.state.runtime.jobs,
            "history": outcome
                .state
                .history
                .entries
                .iter()
                .map(|e| json!({"seq": e.seq, "label": e.label, "cmd": e.cmd}))
                .collect::<Vec<_>>(),
        })
    }

    fn effect_digest(effect: &Effect) -> Value {
        match effect {
            Effect::History(HistoryOp::Append(entry)) => json!({
                "historyAppend": {"seq": entry.seq, "label": entry.label, "cmd": entry.cmd}
            }),
            other => serde_json::to_value(other).expect("effects serialize"),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::fixtures::{self, MIC1, MIC2, SYSTEM1, ms};
    use super::*;
    use crate::error::ErrorCode;
    use crate::ids::JobId;
    use crate::protocol::effect::HistoryOp;
    use crate::protocol::reply::{AutocutPlanView, CmdResult};
    use crate::state::RecordPhase;
    use crate::testkit::{self, ProjectBuilder};

    fn run(
        state: &State,
        ctx: &Ctx,
        scope: ClipScope,
        dry_run: bool,
    ) -> Result<Outcome, CoreError> {
        autocut(state, ctx, scope, AutocutOverrides::default(), dry_run)
    }

    fn clips(state: &State) -> Vec<(&str, u64, u64)> {
        state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| (c.id.as_str(), c.in_, c.out))
            .collect()
    }

    fn plan(result: &CmdResult) -> AutocutPlanView {
        serde_json::from_value(result.result.clone()).unwrap()
    }

    #[test]
    fn known_silences_complete_at_once() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = fixtures::cached(testkit::state(project), &ctx, &fixtures::params());
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();

        let reply = out.reply.clone().expect("completes now").unwrap();
        assert_eq!(reply.seq, Some(1));
        assert_eq!(reply.message, "Autocut 2 takes: removed 4 silences (4.3 s)");
        // Take 1 keeps 0.68–3.12 s and 4.38–8.12 s; take 2 loses 1.12–1.68 s.
        let new = &reply.created;
        assert_eq!(new.len(), 2);
        assert_eq!(
            clips(&out.state),
            vec![
                ("c001", 20, 94),
                (new[0].as_str(), 131, 244),
                ("c002", 0, 34),
                (new[1].as_str(), 50, 150),
            ]
        );
        assert!(reply.removed.is_empty(), "the first fragment keeps the id");
        let view = plan(&reply);
        assert_eq!(view.skipped, vec![]);
        assert_eq!(view.clips.len(), 2);
        assert_eq!(view.clips[0].kept, vec![[20, 94], [131, 244]]);
        assert_eq!(view.clips[0].silences, 3);
        assert_eq!(view.clips[0].removed_frames, 20 + 37 + 56);
        assert_eq!(view.clips[1].kept, vec![[0, 34], [50, 150]]);

        // One history entry for the whole command, with the command as cmd.
        let entries = &out.state.history.entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].label,
            "Autocut 2 takes: removed 4 silences (4.3 s)"
        );
        assert_eq!(
            entries[0].cmd,
            json!({"cmd": "autocut", "scope": "all", "overrides": {}, "dryRun": false})
        );
        assert!(matches!(
            out.effects.as_slice(),
            [
                Effect::History(HistoryOp::Append(_)),
                Effect::WriteProject,
                ..
            ]
        ));
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::DetectSilence { .. })),
            "nothing to analyze"
        );
        assert!(out.state.runtime.jobs.is_empty());
    }

    #[test]
    fn missing_silences_start_a_job() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();
        assert_eq!(out.reply, None, "the reply comes when the analysis is done");
        assert_eq!(out.state.project, state.project);
        assert!(out.state.history.entries.is_empty());

        let p = fixtures::params();
        let detect: Vec<(&SilenceKey, &Vec<String>)> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::DetectSilence { job, key, argv } => {
                    assert_eq!(*job, JobId(1));
                    Some((key, argv))
                }
                _ => None,
            })
            .collect();
        assert_eq!(detect.len(), 2, "one per mic file");
        assert_eq!(detect[0].0, &fixtures::key(&ctx, MIC1, &p));
        assert_eq!(detect[0].1, &silence_argv(MIC1, -35.0, 400));
        assert_eq!(detect[1].0, &fixtures::key(&ctx, MIC2, &p));

        let job = &out.state.runtime.jobs[&JobId(1)];
        assert_eq!(job.req, ctx.req);
        assert_eq!(job.started, ctx.now);
        let JobKind::Autocut {
            targets,
            pending,
            params,
            dry_run,
            ready,
            cmd,
        } = &job.kind
        else {
            panic!("an autocut job: {job:?}");
        };
        assert_eq!(params, &p);
        assert!(!dry_run && !ready);
        assert_eq!(pending.len(), 2);
        assert_eq!(targets.len(), 2);
        assert_eq!(
            targets[0].clip,
            ClipRef::of(&state.project.timeline.clips[0])
        );
        assert_eq!(targets[0].track.as_str(), "A1");
        assert_eq!(cmd["cmd"], "autocut");
        assert!(cmd.get("skipped").is_none());
        assert!(out.effects.iter().any(|e| matches!(
            e,
            Effect::Progress { progress, .. } if progress.label == "Analyzing audio 0/2"
        )));
    }

    #[test]
    fn only_unknown_keys_are_analyzed() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let p = fixtures::params();
        let mut state = testkit::state(project);
        state
            .runtime
            .silence
            .insert(fixtures::key(&ctx, MIC1, &p), fixtures::silences(MIC1));
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();
        let keys: Vec<&SilenceKey> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::DetectSilence { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![&fixtures::key(&ctx, MIC2, &p)]);
        let progress = out
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Progress { progress, .. } => Some(progress),
                _ => None,
            })
            .expect("progress for the requester");
        assert_eq!((progress.done, progress.total), (1, 2));
    }

    #[test]
    fn a_dry_run_applies_nothing() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = fixtures::cached(testkit::state(project), &ctx, &fixtures::params());
        let out = run(&state, &ctx, ClipScope::All, true).unwrap();
        let reply = out.reply.clone().unwrap().unwrap();
        assert_eq!(reply.seq, None);
        assert_eq!(
            reply.message,
            "Dry run: autocut 2 takes would remove 4 silences (4.3 s)"
        );
        assert_eq!(plan(&reply).clips[0].kept, vec![[20, 94], [131, 244]]);
        assert_eq!(out.state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn nothing_to_cut_appends_no_entry() {
        let project = ProjectBuilder::new()
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 100)
            .build();
        let ctx = fixtures::ctx(&project);
        let mut state = testkit::state(project);
        // The take's only silence is after the clip.
        state.runtime.silence.insert(
            fixtures::key(&ctx, MIC1, &fixtures::params()),
            vec![ms(5000, 9000)],
        );
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();
        let reply = out.reply.unwrap().unwrap();
        assert_eq!(reply.seq, None);
        assert_eq!(
            reply.message,
            "Nothing to cut: no silence long enough to cut at \u{2212}35 dB"
        );
        assert_eq!(plan(&reply).clips[0].kept, vec![[0, 100]]);
        assert!(out.effects.is_empty());
        assert_eq!(out.state.project, state.project);
    }

    #[test]
    fn clips_that_cant_be_analyzed_are_skipped() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 90)
            .mic("A1", "alsa_input.mic")
            .take("take-0002", 150)
            .take("take-0003", 60)
            .with_take("take-0003", |t| t.scene.audio[0].missing = true)
            .clip("c001", "take-0001", 0, 90)
            .clip("c002", "take-0002", 0, 150)
            .clip("c003", "take-0003", 0, 60)
            .build();
        let ctx = fixtures::ctx(&project);
        let mut state = testkit::state(project);
        state.runtime.silence.insert(
            fixtures::key(&ctx, MIC2, &fixtures::params()),
            fixtures::silences(MIC2),
        );
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();
        let reply = out.reply.unwrap().unwrap();
        let view = plan(&reply);
        assert_eq!(
            view.skipped,
            vec![
                AutocutSkipView {
                    clip: ClipId::from_raw("c001"),
                    reason: "take 1 has no audio track".into()
                },
                AutocutSkipView {
                    clip: ClipId::from_raw("c003"),
                    reason: "track A1 of take 3 is missing media".into()
                },
            ]
        );
        assert_eq!(
            reply.message,
            "Autocut take 2: removed 1 silence (0.5 s); 2 clips skipped \
             (c001: take 1 has no audio track, c003: track A1 of take 3 is missing media)"
        );
        assert_eq!(out.state.history.entries.len(), 1);

        // Every clip skipped: nothing to cut, still a success.
        let only = run(
            &state,
            &ctx,
            ClipScope::Clips(vec![ClipId::from_raw("c001")]),
            false,
        )
        .unwrap()
        .reply
        .unwrap()
        .unwrap();
        assert_eq!(
            only.message,
            "Nothing to cut: no clip could be analyzed; 1 clip skipped \
             (c001: take 1 has no audio track)"
        );
        assert_eq!(only.seq, None);
    }

    #[test]
    fn a_file_without_facts_is_skipped() {
        let project = fixtures::project();
        let full = fixtures::ctx(&project);
        let mut ctx = full.clone();
        ctx.facts.files.remove(MIC2);
        let state = fixtures::cached(testkit::state(project), &full, &fixtures::params());
        let reply = run(&state, &ctx, ClipScope::All, false)
            .unwrap()
            .reply
            .unwrap()
            .unwrap();
        assert_eq!(
            plan(&reply).skipped,
            vec![AutocutSkipView {
                clip: ClipId::from_raw("c002"),
                reason: format!("audio file {MIC2} not found"),
            }]
        );
    }

    #[test]
    fn an_entirely_silent_clip_is_left_alone() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let mut state = fixtures::cached(testkit::state(project), &ctx, &fixtures::params());
        state.runtime.silence.insert(
            fixtures::key(&ctx, MIC2, &fixtures::params()),
            vec![ms(0, 5000)],
        );
        let out = run(&state, &ctx, ClipScope::All, false).unwrap();
        let reply = out.reply.unwrap().unwrap();
        let view = plan(&reply);
        assert_eq!(
            view.skipped,
            vec![AutocutSkipView {
                clip: ClipId::from_raw("c002"),
                reason: "entirely silent at \u{2212}35 dB".into()
            }]
        );
        assert_eq!(view.clips.len(), 1);
        assert!(
            reply
                .message
                .starts_with("Autocut take 1: removed 3 silences")
        );
        assert_eq!(
            out.state.project.timeline.clips[2],
            state.project.timeline.clips[1]
        );
    }

    #[test]
    fn overrides_pick_the_track_and_parameters() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let overrides = AutocutOverrides {
            track: Some("A2".parse().unwrap()),
            threshold_db: Some(-42.5),
            min_silence_ms: Some(700),
            padding_ms: Some(50),
            min_keep_ms: Some(0),
        };
        let scope = ClipScope::Clips(vec![ClipId::from_raw("c001")]);
        let out = autocut(&state, &ctx, scope.clone(), overrides.clone(), false).unwrap();
        let argv: Vec<&Vec<String>> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::DetectSilence { argv, .. } => Some(argv),
                _ => None,
            })
            .collect();
        assert_eq!(argv, vec![&silence_argv(SYSTEM1, -42.5, 700)]);
        let JobKind::Autocut {
            params,
            targets,
            cmd,
            ..
        } = &out.state.runtime.jobs[&JobId(1)].kind
        else {
            panic!("an autocut job");
        };
        assert_eq!(
            params,
            &AutocutParams {
                threshold_db: -42.5,
                min_silence_ms: 700,
                padding_ms: 50,
                min_keep_ms: 0
            }
        );
        assert_eq!(targets[0].track.as_str(), "A2");
        let original = Command::Autocut {
            scope,
            overrides,
            dry_run: false,
        };
        assert_eq!(cmd, &original.to_json());
    }

    #[test]
    fn bad_arguments_change_nothing() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let with = |o: AutocutOverrides| autocut(&state, &ctx, ClipScope::All, o, false);
        let cases = [
            (
                AutocutOverrides {
                    threshold_db: Some(3.0),
                    ..Default::default()
                },
                ErrorCode::Usage,
                "the autocut threshold must be between \u{2212}90 and 0 dB (got 3 dB)",
            ),
            (
                AutocutOverrides {
                    threshold_db: Some(-120.0),
                    ..Default::default()
                },
                ErrorCode::Usage,
                "the autocut threshold must be between \u{2212}90 and 0 dB (got \u{2212}120 dB)",
            ),
            (
                AutocutOverrides {
                    threshold_db: Some(f64::NAN),
                    ..Default::default()
                },
                ErrorCode::Usage,
                "the autocut threshold must be between \u{2212}90 and 0 dB (got NaN dB)",
            ),
            (
                AutocutOverrides {
                    padding_ms: Some(60_001),
                    ..Default::default()
                },
                ErrorCode::Usage,
                "the autocut padding must be at most 60000 ms (got 60001 ms)",
            ),
            (
                AutocutOverrides {
                    min_silence_ms: Some(90_000),
                    ..Default::default()
                },
                ErrorCode::Usage,
                "the autocut minimum silence must be at most 60000 ms (got 90000 ms)",
            ),
            (
                AutocutOverrides {
                    track: Some("A9".parse().unwrap()),
                    ..Default::default()
                },
                ErrorCode::NotFound,
                "no audio track A9",
            ),
        ];
        for (overrides, code, text) in cases {
            let err = with(overrides).unwrap_err();
            assert_eq!((err.code, err.message.as_str()), (code, text));
        }
        let unknown = ClipScope::Clips(vec![ClipId::from_raw("c404")]);
        let err = run(&state, &ctx, unknown, false).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotFound, "no clip c404 on the timeline")
        );
        let err = run(&state, &ctx, ClipScope::Clips(vec![]), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        let empty = testkit::state(ProjectBuilder::new().mic("A1", "m").build());
        let err = run(&empty, &ctx, ClipScope::All, false).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (
                ErrorCode::Invalid,
                "nothing to autocut: the timeline is empty"
            )
        );
        // A hand-edited out-of-range setting is caught too.
        let mut odd = state.clone();
        odd.project.autocut.min_keep_ms = 70_000;
        let err = run(&odd, &ctx, ClipScope::All, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
    }

    #[test]
    fn scope_follows_timeline_order_without_duplicates() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let scope = ClipScope::Clips(vec![
            ClipId::from_raw("c002"),
            ClipId::from_raw("c001"),
            ClipId::from_raw("c002"),
        ]);
        let out = run(&state, &ctx, scope, false).unwrap();
        let JobKind::Autocut { targets, .. } = &out.state.runtime.jobs[&JobId(1)].kind else {
            panic!("an autocut job");
        };
        let order: Vec<&str> = targets.iter().map(|t| t.clip.id.as_str()).collect();
        assert_eq!(order, ["c001", "c002"]);
    }

    #[test]
    fn auto_run_after_take() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let mut state = testkit::state(project);
        let c002 = ClipId::from_raw("c002");
        assert_eq!(
            start_auto_run(&state, &ctx, &c002),
            Outcome::unchanged(&state),
            "off by default"
        );

        state.project.autocut.auto_run_after_take = true;
        let out = start_auto_run(&state, &ctx, &c002);
        assert_eq!(out.reply, None);
        let job = &out.state.runtime.jobs[&JobId(1)];
        assert_eq!(job.req, None, "nobody waits");
        let JobKind::Autocut { cmd, targets, .. } = &job.kind else {
            panic!("an autocut job");
        };
        assert_eq!(
            cmd,
            &json!({"event": "autoRunAfterTake", "take": "take-0002", "clip": "c002"})
        );
        assert_eq!(targets.len(), 1);
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::Progress { .. }))
        );

        // Known silences: applied at once, told with a toast.
        let state = fixtures::cached(state, &ctx, &fixtures::params());
        let out = start_auto_run(&state, &ctx, &c002);
        let label = "Autocut take 2: removed 1 silence (0.5 s)";
        assert_eq!(out.state.history.entries.len(), 1);
        assert_eq!(out.state.history.entries[0].label, label);
        assert!(matches!(
            out.effects.last(),
            Some(Effect::Notify(toast)) if toast.title == label
        ));
        // A clip that isn't there (any more) is only logged.
        let gone = start_auto_run(&state, &ctx, &ClipId::from_raw("c999"));
        assert_eq!(gone.state, state);
        assert!(matches!(gone.effects.as_slice(), [Effect::Log { .. }]));
    }

    #[test]
    fn ready_jobs_are_applied_before_a_new_run() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let p = fixtures::params();
        let mut state = fixtures::cached(testkit::state(project), &ctx, &p);
        // A job that finished analyzing during a recording that placed nothing.
        let job = Job {
            id: JobId(9),
            kind: JobKind::Autocut {
                cmd: json!({"cmd": "autocut", "scope": {"clips": ["c002"]}, "overrides": {}, "dryRun": false}),
                targets: vec![AutocutTarget {
                    clip: ClipRef::of(&state.project.timeline.clips[1]),
                    track: "A1".parse().unwrap(),
                    key: fixtures::key(&ctx, MIC2, &p),
                }],
                params: p.clone(),
                dry_run: false,
                pending: BTreeSet::new(),
                ready: true,
            },
            req: Some(ReqId("r0".into())),
            started: ctx.now,
        };
        state.runtime.jobs.insert(JobId(9), job);
        let scope = ClipScope::Clips(vec![ClipId::from_raw("c001")]);
        let out = run(&state, &ctx, scope, false).unwrap();
        let labels: Vec<&str> = out
            .state
            .history
            .entries
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Autocut take 2: removed 1 silence (0.5 s)",
                "Autocut take 1: removed 3 silences (3.8 s)"
            ]
        );
        assert!(out.effects.iter().any(|e| matches!(
            e,
            Effect::Reply { req, reply: Ok(_) } if req.0 == "r0"
        )));
        assert_eq!(out.reply.unwrap().unwrap().seq, Some(2));
        assert!(out.state.runtime.jobs.is_empty());

        // A dry run leaves the ready job alone.
        let out = run(&state, &ctx, ClipScope::All, true).unwrap();
        assert!(out.state.runtime.jobs.contains_key(&JobId(9)));
    }

    #[test]
    fn outcomes_snapshot() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let started = run(&state, &ctx, ClipScope::All, false).unwrap();
        let cached = fixtures::cached(state, &ctx, &fixtures::params());
        let at_once = run(&cached, &ctx, ClipScope::All, false).unwrap();
        let only_c002 = ClipScope::Clips(vec![ClipId::from_raw("c002")]);
        let dry = run(&cached, &ctx, only_c002, true).unwrap();
        insta::assert_yaml_snapshot!(json!({
            "started": fixtures::digest(&started),
            "completedAtOnce": fixtures::digest(&at_once),
            "dryRun": fixtures::digest(&dry),
        }));
    }

    #[test]
    fn the_command_is_refused_while_recording() {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let mut state = testkit::state(project);
        state.runtime.record = RecordPhase::Recording {
            take: crate::ids::TakeId::numbered(3),
            mode: Default::default(),
            elapsed_ns: 0,
            waiters: vec![],
        };
        let cmd = Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run: false,
        };
        let err = crate::apply::apply(&state, &ctx, cmd).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
    }
}
