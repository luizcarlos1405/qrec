//! `Effect::DetectSilence` → `SilenceDetected` | `SilenceFailed` (SPEC §9,
//! A§5.7). Results are cached in `.qrec/cache/silence/<key>.json`; the key
//! already names the file's size and mtime and the parameters, so a cached
//! result is reused as is. Otherwise the core-built `silencedetect` argv
//! runs with its stderr in a job log, and `parse::silencedetect` reads it.

use std::fs;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use qrec_core::parse::silencedetect::parse_silencedetect;
use qrec_core::protocol::Interval;
use qrec_core::state::SilenceKey;
use qrec_core::{Event, JobId};
use serde::{Deserialize, Serialize};

use super::Ctx;
use super::child::{self, Spec, Stderr};
use super::jobs::JobCtl;
use super::logs::tail_lines;
use super::place::temp_path;

/// Bumped when the cache file's meaning changes.
const CACHE_VERSION: u32 = 1;

/// `.qrec/cache/silence/<key>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CacheFile {
    pub version: u32,
    pub key: String,
    pub silences: Vec<Interval>,
}

pub(crate) fn run(
    ctx: &Ctx,
    ctl: &JobCtl,
    job: JobId,
    key: SilenceKey,
    argv: &[String],
) -> Event {
    match detect(ctx, ctl, job, &key, argv) {
        Ok(silences) => Event::SilenceDetected { job, key, silences },
        Err(message) => Event::SilenceFailed { job, key, message },
    }
}

/// The cache file of `key`; `None` for keys that aren't plain file names.
pub(crate) fn cache_path(ctx: &Ctx, key: &SilenceKey) -> Option<PathBuf> {
    let ok = !key.0.is_empty()
        && key.0.len() <= 128
        && key
            .0
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| ctx.project.silence_cache_dir().join(format!("{}.json", key.0)))
}

/// A cached result, if a valid one exists.
pub(crate) fn read_cache(path: &Path, key: &SilenceKey) -> Option<Vec<Interval>> {
    let text = fs::read_to_string(path).ok()?;
    let cache: CacheFile = serde_json::from_str(&text).ok()?;
    let valid = cache.version == CACHE_VERSION
        && cache.key == key.0
        && cache.silences.iter().all(|i| i.start_ns <= i.end_ns);
    valid.then_some(cache.silences)
}

/// Writes the cache atomically (temp + rename). Best effort.
fn write_cache(ctx: &Ctx, job: JobId, path: &Path, key: &SilenceKey, silences: &[Interval]) {
    let Some(dir) = path.parent() else { return };
    let cache = CacheFile {
        version: CACHE_VERSION,
        key: key.0.clone(),
        silences: silences.to_vec(),
    };
    let tmp = temp_path(dir, &key.0, job, "json");
    let result = (|| -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(&cache).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        // Same key ⇒ same content: replacing a concurrent writer's file is fine.
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        tracing::warn!(
            "can't cache the silences of {} in {}: {e}",
            key.0,
            ctx.project.silence_cache_dir().display()
        );
    }
}

fn detect(
    ctx: &Ctx,
    ctl: &JobCtl,
    job: JobId,
    key: &SilenceKey,
    argv: &[String],
) -> Result<Vec<Interval>, String> {
    let cache = cache_path(ctx, key);
    if let Some(silences) = cache.as_deref().and_then(|p| read_cache(p, key)) {
        return Ok(silences);
    }
    if ctl.is_cancelled() {
        return Err("cancelled".to_owned());
    }
    let input_bytes = input_size(ctx, argv);
    let log = ctx.open_log(&ctx.job_log_base(job, "silence"), argv)?;
    let program = argv.first().map_or("ffmpeg", String::as_str);
    let done = child::run(
        Spec::new(argv, Stderr::Log(log.file()))
            .cwd(ctx.root())
            .timeout(ctx.options.scaled(ctx.options.silence_timeout, input_bytes))
            .stop(&ctl.stop)
            .kill_grace(ctx.options.kill_grace),
    )?;
    if ctl.is_cancelled() {
        return Err("cancelled".to_owned());
    }
    let stderr = log.body();
    if !done.exited_ok() {
        let mut message = done.describe(program);
        if let Some(last) = tail_lines(&stderr, 1).pop() {
            message.push_str(&format!(": {last}"));
        }
        return Err(format!("{message} (log: {})", log.display()));
    }
    let parsed = catch_unwind(AssertUnwindSafe(|| parse_silencedetect(&stderr))).map_err(
        |panic| {
            format!(
                "internal error: the silencedetect parser failed: {}",
                child::panic_message(panic.as_ref())
            )
        },
    )?;
    let silences = parsed.map_err(|e| format!("{e} (log: {})", log.display()))?;
    if let Some(path) = &cache {
        write_cache(ctx, job, path, key, &silences);
    }
    Ok(silences)
}

/// The size of the argv's input (`-i file:<path>`), for the timeout.
fn input_size(ctx: &Ctx, argv: &[String]) -> u64 {
    argv.windows(2)
        .filter(|w| w[0] == "-i")
        .filter_map(|w| {
            let path = w[1].strip_prefix("file:").unwrap_or(&w[1]);
            let path = Path::new(path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                ctx.root().join(path)
            };
            fs::metadata(path).ok().map(|m| m.len())
        })
        .sum()
}
