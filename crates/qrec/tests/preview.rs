//! The GUI's preview cache on the design fixture (docs/GUI.md G§3.11, G§7
//! WP2): `CachedSilence` reads the silence analysis that the real `qrec
//! autocut --dry-run` cached, for the project's own threshold and minimum
//! silence, and never runs ffmpeg for it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use qrec_core::TakeId;
use qrec_core::state::SilenceKey;
use qrec_shell::media::preview::{PreviewCache, PreviewData, PreviewRequest};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use qrec_shell::testkit::{TestEnv, run_tool};
use serde_json::Value;

/// Runs `qrec <args>` in `dir`; the one `--json` object it printed.
fn qrec_json(env: &TestEnv, dir: &Path, args: &[&str]) -> Value {
    let mut command = env.command(&PathBuf::from(env!("CARGO_BIN_EXE_qrec")));
    command.current_dir(dir).args(args);
    let out = run_tool(&mut command, Duration::from_secs(120));
    let stdout = String::from_utf8(out.stdout).expect("UTF-8");
    assert!(
        out.status.success(),
        "qrec {args:?}: {stdout}\n{}",
        out.stderr
    );
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{stdout}: {e}"))
}

fn ask(cache: &PreviewCache, rx: &Receiver<PreviewData>, req: PreviewRequest) -> PreviewData {
    cache.request(req);
    rx.recv_timeout(Duration::from_secs(60)).expect("an answer")
}

#[test]
fn cached_silence_reads_what_autocut_dry_run_analyzed() {
    let env = TestEnv::new();
    let dir = env.empty_dir("rust-traits-talk");
    let project = design_fixture(&dir, DesignVariant::Base);
    // c2d0 is take 5; its A1 holds BRIEF §3.3's two silences.
    let take = &project.takes[&TakeId::numbered(5)];
    let a1 = take.stream("A1").expect("take 5 has A1").file.clone();
    let (threshold_db, min_silence_ms) =
        (project.autocut.threshold_db, project.autocut.min_silence_ms);
    let request = PreviewRequest::CachedSilence {
        file: a1.clone(),
        threshold_db,
        min_silence_ms,
    };

    let (tx, rx) = channel();
    let cache = PreviewCache::new(
        dir.clone(),
        Box::new(move |data| {
            let _ = tx.send(data);
        }),
    );
    let PreviewData::Silence {
        key,
        intervals: None,
        ..
    } = ask(&cache, &rx, request.clone())
    else {
        panic!("nothing is analyzed before the dry run");
    };

    let dry = qrec_json(
        &env,
        &dir,
        &["autocut", "--clips", "c2d0", "--dry-run", "--json"],
    );
    assert_eq!(dry["data"]["result"]["clips"][0]["silences"], 2, "{dry}");

    let data = ask(&cache, &rx, request);
    let PreviewData::Silence {
        file,
        key: after,
        intervals: Some(silences),
    } = &data
    else {
        panic!("the dry run's analysis is cached: {data:?}");
    };
    assert_eq!(file, &a1);
    assert_eq!(after, &key, "same file version, same parameters");
    let fact = std::fs::metadata(dir.join(&a1)).unwrap();
    use std::os::unix::fs::MetadataExt;
    let mtime_ns = fact.mtime() * 1_000_000_000 + fact.mtime_nsec();
    assert_eq!(
        key,
        SilenceKey::compute(&a1, fact.len(), mtime_ns, threshold_db, min_silence_ms)
    );
    // BRIEF §3.3: take 5's A1 is silent twice, at 4.000–5.267 s and
    // 9.600–10.100 s of the file (00:01:13:02–00:01:14:10 and
    // 00:01:18:20–00:01:19:05 on the timeline, inside c2d0).
    let ms: Vec<(i64, i64)> = silences
        .iter()
        .map(|s| (s.start_ns / 1_000_000, s.end_ns / 1_000_000))
        .collect();
    assert_eq!(ms.len(), 2, "{ms:?}");
    for ((start, end), (want_start, want_end)) in ms.iter().zip([(4_000, 5_267), (9_600, 10_100)]) {
        assert!(
            (start - want_start).abs() <= 20 && (end - want_end).abs() <= 20,
            "{ms:?}"
        );
    }

    // Other parameters were never analyzed.
    let PreviewData::Silence { intervals, .. } = ask(
        &cache,
        &rx,
        PreviewRequest::CachedSilence {
            file: a1,
            threshold_db: threshold_db - 10.0,
            min_silence_ms,
        },
    ) else {
        panic!()
    };
    assert_eq!(intervals, None);
    // No ffmpeg ran for any of it (nothing was written to the preview log).
    assert!(!dir.join(".qrec/logs/preview.log").exists());
}
