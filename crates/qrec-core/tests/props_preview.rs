//! Properties of the GUI's waveform peaks (docs/GUI.md G§3.11,
//! `media_argv`): one bucket per millisecond, each coarser bucket is the
//! min and the max of its finer ones, bucketing as samples arrive gives
//! the buckets of the whole stream, and the level a lane draws from has a
//! bucket per pixel. Preview keys are deterministic and follow every input.

use proptest::prelude::*;

use qrec_core::media_argv::{
    PEAK_LEVEL_STEP, PEAK_SAMPLES_PER_MS, PreviewKind, bucket_peaks, peak_levels, preview_key,
};

/// (min, max) of `buckets`, as one coarser bucket.
fn fold(buckets: &[(i16, i16)]) -> (i16, i16) {
    buckets
        .iter()
        .fold((i16::MAX, i16::MIN), |(lo, hi), &(a, b)| {
            (lo.min(a), hi.max(b))
        })
}

fn arb_samples() -> impl Strategy<Value = Vec<i16>> {
    prop_oneof![
        // Short runs around the bucket and level boundaries.
        prop::collection::vec(any::<i16>(), 0..400),
        // Up to a few hundred ms of quieter audio, so levels have depth.
        prop::collection::vec(-2000i16..2000, 0..(PEAK_SAMPLES_PER_MS * 300)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn one_bucket_per_millisecond(samples in arb_samples()) {
        let per_ms = bucket_peaks(&samples, PEAK_SAMPLES_PER_MS);
        prop_assert_eq!(per_ms.len(), samples.len().div_ceil(PEAK_SAMPLES_PER_MS));
        for (i, &(lo, hi)) in per_ms.iter().enumerate() {
            let run = &samples[i * PEAK_SAMPLES_PER_MS..((i + 1) * PEAK_SAMPLES_PER_MS).min(samples.len())];
            prop_assert_eq!(lo, *run.iter().min().unwrap());
            prop_assert_eq!(hi, *run.iter().max().unwrap());
        }
    }

    /// Each coarser bucket is the min and the max of its finer ones: 16 of
    /// them (the last of fewer), and so 256 of the finest.
    #[test]
    fn coarser_buckets_are_the_min_and_max_of_their_finer_ones(
        per_ms in prop::collection::vec((any::<i16>(), any::<i16>()), 0..1500),
    ) {
        let per_ms: Vec<(i16, i16)> = per_ms
            .into_iter()
            .map(|(a, b)| (a.min(b), a.max(b)))
            .collect();
        let levels = peak_levels(per_ms.clone().into());
        prop_assert_eq!(&levels.per_ms[..], &per_ms[..]);
        let step = PEAK_LEVEL_STEP;
        prop_assert_eq!(levels.per_16ms.len(), per_ms.len().div_ceil(step));
        prop_assert_eq!(levels.per_256ms.len(), per_ms.len().div_ceil(step * step));
        for (j, &coarse) in levels.per_16ms.iter().enumerate() {
            let finer = &per_ms[j * step..((j + 1) * step).min(per_ms.len())];
            prop_assert_eq!(coarse, fold(finer));
        }
        for (j, &coarse) in levels.per_256ms.iter().enumerate() {
            let finer = &levels.per_16ms[j * step..((j + 1) * step).min(levels.per_16ms.len())];
            prop_assert_eq!(coarse, fold(finer));
            let finest = &per_ms[j * step * step..((j + 1) * step * step).min(per_ms.len())];
            prop_assert_eq!(coarse, fold(finest));
        }
        // Every coarse bucket holds its finer ones.
        for (i, &(lo, hi)) in per_ms.iter().enumerate() {
            let (clo, chi) = levels.per_16ms[i / step];
            prop_assert!(clo <= lo && hi <= chi);
            let (slo, shi) = levels.per_256ms[i / (step * step)];
            prop_assert!(slo <= lo && hi <= shi);
        }
    }

    /// The shell buckets ffmpeg's samples as they arrive: whole buckets at a
    /// time, the rest with the next read. That is the whole stream's buckets.
    #[test]
    fn bucketing_as_samples_arrive_is_bucketing_the_whole(
        samples in arb_samples(),
        cuts in prop::collection::vec(0usize..20, 0..8),
    ) {
        let whole = bucket_peaks(&samples, PEAK_SAMPLES_PER_MS);
        let mut streamed = Vec::new();
        let mut at = 0;
        for cut in cuts {
            let end = (at + cut * PEAK_SAMPLES_PER_MS).min(samples.len());
            let end = at + (end - at) / PEAK_SAMPLES_PER_MS * PEAK_SAMPLES_PER_MS;
            streamed.extend(bucket_peaks(&samples[at..end], PEAK_SAMPLES_PER_MS));
            at = end;
        }
        streamed.extend(bucket_peaks(&samples[at..], PEAK_SAMPLES_PER_MS));
        prop_assert_eq!(streamed, whole);
    }

    /// A lane draws from the coarsest level with at least one bucket per
    /// pixel.
    #[test]
    fn the_lanes_level_has_a_bucket_per_pixel(ms_per_px in 0.01f64..100_000.0) {
        let levels = peak_levels(vec![(0, 0); 600].into());
        let (bucket_ms, buckets) = levels.level(ms_per_px);
        prop_assert!(f64::from(bucket_ms) <= ms_per_px.max(1.0));
        let coarser = f64::from(bucket_ms) * PEAK_LEVEL_STEP as f64;
        prop_assert!(bucket_ms == 256 || coarser > ms_per_px, "a coarser level would do");
        prop_assert_eq!(buckets.len(), 600usize.div_ceil(bucket_ms as usize));
    }

    #[test]
    fn preview_keys_are_deterministic(
        file in "[a-z/._-]{1,40}",
        size in any::<u64>(),
        mtime in any::<i64>(),
        chunk in 0u32..100,
        other in 0u32..100,
    ) {
        let params = format!("chunk={chunk} height=72");
        let key = preview_key(PreviewKind::Strip, &file, size, mtime, &params);
        prop_assert_eq!(&key, &preview_key(PreviewKind::Strip, &file, size, mtime, &params));
        prop_assert_eq!(key.len(), 64);
        let other_params = format!("chunk={other} height=72");
        prop_assert_eq!(
            key == preview_key(PreviewKind::Strip, &file, size, mtime, &other_params),
            chunk == other
        );
        prop_assert_ne!(&key, &preview_key(PreviewKind::Peaks, &file, size, mtime, &params));
        prop_assert_ne!(&key, &preview_key(PreviewKind::Strip, &file, size ^ 1, mtime, &params));
        prop_assert_ne!(&key, &preview_key(PreviewKind::Strip, &file, size, mtime ^ 1, &params));
    }
}
