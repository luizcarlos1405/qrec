use crate::config::{Chunk, QrecConfig};
use chrono::Utc;

pub fn add_chunk(mut config: QrecConfig, file: String, duration_secs: f64) -> QrecConfig {
    let id = config.generate_chunk_id();
    let chunk = Chunk {
        id,
        file,
        duration_secs,
        recorded_at: Utc::now(),
        trim_start: None,
        trim_end: None,
    };
    config.chunks.push(chunk);
    config.next_chunk_number += 1;
    config
}

pub fn remove_chunk(mut config: QrecConfig, index: usize) -> (QrecConfig, Option<String>) {
    if index < config.chunks.len() {
        let chunk = config.chunks.remove(index);
        (config, Some(chunk.file.clone()))
    } else {
        (config, None)
    }
}

pub fn move_chunk_left(mut config: QrecConfig, index: usize) -> (QrecConfig, bool) {
    if index == 0 || index >= config.chunks.len() {
        return (config, false);
    }
    config.chunks.swap(index, index - 1);
    (config, true)
}

pub fn move_chunk_right(mut config: QrecConfig, index: usize) -> (QrecConfig, bool) {
    if index + 1 >= config.chunks.len() {
        return (config, false);
    }
    config.chunks.swap(index, index + 1);
    (config, true)
}

pub fn chunk_char_width(duration_secs: f64, fps: f64, frames_per_char: usize) -> usize {
    if frames_per_char == 0 {
        return 1;
    }
    let frames = duration_secs * fps;
    let width = (frames / frames_per_char as f64).ceil() as usize;
    width.max(1)
}

pub fn total_frames(chunks: &[Chunk], fps: f64) -> f64 {
    chunks.iter().map(|c| c.duration_secs * fps).sum()
}

pub fn default_frames_per_char(chunks: &[Chunk], fps: f64, viewport_width: usize) -> usize {
    if chunks.is_empty() || viewport_width == 0 {
        return 1;
    }
    let total = total_frames(chunks, fps);
    let fpc = (total / viewport_width as f64).ceil() as usize;
    if fpc == 0 {
        return 1;
    }
    fpc.next_power_of_two()
}

pub fn zoom_in(frames_per_char: usize) -> usize {
    (frames_per_char / 2).max(1)
}

pub fn zoom_out(frames_per_char: usize) -> usize {
    (frames_per_char * 2).max(1)
}

pub fn chunk_start_col(
    config: &QrecConfig,
    index: usize,
    fps: f64,
    frames_per_char: usize,
) -> usize {
    let mut col = 0;
    for (i, chunk) in config.chunks.iter().enumerate() {
        if i == index {
            break;
        }
        col += chunk_char_width(chunk.duration_secs, fps, frames_per_char);
    }
    col
}

pub fn chunk_cut_mask(
    duration_secs: f64,
    trim_start: f64,
    trim_end: f64,
    fps: f64,
    frames_per_char: usize,
) -> Vec<bool> {
    let w = chunk_char_width(duration_secs, fps, frames_per_char);
    let has_trim = trim_start > 0.01 || (duration_secs - trim_end).abs() > 0.01;
    if !has_trim {
        return vec![false; w];
    }
    let mut mask = Vec::with_capacity(w);
    for i in 0..w {
        let char_start = i as f64 * frames_per_char as f64 / fps;
        let char_end = ((i + 1) as f64 * frames_per_char as f64 / fps).min(duration_secs);
        let cut = char_start < trim_start || char_end > trim_end;
        mask.push(cut);
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Chunk;
    use chrono::Utc;

    fn make_chunk(file: &str, duration: f64) -> Chunk {
        Chunk {
            id: file.to_string(),
            file: file.to_string(),
            duration_secs: duration,
            recorded_at: Utc::now(),
            trim_start: None,
            trim_end: None,
        }
    }

    #[test]
    fn add_chunk_appends_and_returns_config() {
        let config = QrecConfig::default();
        let config = add_chunk(config, "chunk-1.mp4".to_string(), 5.0);
        assert_eq!(config.chunks.len(), 1);
        assert_eq!(config.chunks[0].file, "chunk-1.mp4");
        assert_eq!(config.next_chunk_number, 2);
    }

    #[test]
    fn remove_chunk_returns_file() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        config.chunks.push(make_chunk("b.mp4", 2.0));
        let (config, file) = remove_chunk(config, 0);
        assert_eq!(config.chunks.len(), 1);
        assert_eq!(file, Some("a.mp4".to_string()));
    }

    #[test]
    fn remove_chunk_out_of_bounds_returns_none() {
        let config = QrecConfig::default();
        let (config, file) = remove_chunk(config, 5);
        assert_eq!(config.chunks.len(), 0);
        assert_eq!(file, None);
    }

    #[test]
    fn move_chunk_left_swaps() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        config.chunks.push(make_chunk("b.mp4", 2.0));
        let (config, moved) = move_chunk_left(config, 1);
        assert!(moved);
        assert_eq!(config.chunks[0].file, "b.mp4");
        assert_eq!(config.chunks[1].file, "a.mp4");
    }

    #[test]
    fn move_chunk_left_at_zero_fails() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        let (config, moved) = move_chunk_left(config, 0);
        assert!(!moved);
        assert_eq!(config.chunks[0].file, "a.mp4");
    }

    #[test]
    fn move_chunk_right_swaps() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        config.chunks.push(make_chunk("b.mp4", 2.0));
        let (config, moved) = move_chunk_right(config, 0);
        assert!(moved);
        assert_eq!(config.chunks[0].file, "b.mp4");
        assert_eq!(config.chunks[1].file, "a.mp4");
    }

    #[test]
    fn move_chunk_right_at_end_fails() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        config.chunks.push(make_chunk("b.mp4", 2.0));
        let (_config, moved) = move_chunk_right(config, 1);
        assert!(!moved);
    }

    #[test]
    fn zoom_in_halves() {
        assert_eq!(zoom_in(4), 2);
        assert_eq!(zoom_in(1), 1);
    }

    #[test]
    fn zoom_out_doubles() {
        assert_eq!(zoom_out(2), 4);
        assert_eq!(zoom_out(1), 2);
    }

    #[test]
    fn chunk_char_width_minimum_one() {
        assert_eq!(chunk_char_width(0.0, 30.0, 1), 1);
        assert_eq!(chunk_char_width(1.0, 30.0, 0), 1);
    }

    #[test]
    fn chunk_char_width_computes_correctly() {
        let width = chunk_char_width(1.0, 30.0, 10);
        assert_eq!(width, 3);
    }

    #[test]
    fn default_frames_per_char_empty_chunks() {
        assert_eq!(default_frames_per_char(&[], 30.0, 80), 1);
    }

    #[test]
    fn total_frames_sums() {
        let chunks = vec![make_chunk("a.mp4", 2.0), make_chunk("b.mp4", 3.0)];
        assert_eq!(total_frames(&chunks, 30.0), 150.0);
    }

    #[test]
    fn chunk_start_col_offsets() {
        let mut config = QrecConfig::default();
        config.chunks.push(make_chunk("a.mp4", 1.0));
        config.chunks.push(make_chunk("b.mp4", 2.0));
        assert_eq!(chunk_start_col(&config, 0, 30.0, 10), 0);
        assert_eq!(chunk_start_col(&config, 1, 30.0, 10), 3);
    }

    #[test]
    fn cut_mask_no_trimming() {
        let mask = chunk_cut_mask(10.0, 0.0, 10.0, 30.0, 30);
        assert!(mask.iter().all(|&c| !c));
    }

    #[test]
    fn cut_mask_leading_trim_only() {
        // 10s, 30fps, 30 frames_per_char = 10 chars total
        // trim_start=3.0s means first 3 chars (0..3s) are cut
        let mask = chunk_cut_mask(10.0, 3.0, 10.0, 30.0, 30);
        assert_eq!(mask.len(), 10);
        assert!(mask[0] && mask[1] && mask[2]);
        assert!(!mask[3] && !mask[9]);
    }

    #[test]
    fn cut_mask_trailing_trim_only() {
        // 10s, 30fps, 30 frames_per_char = 10 chars
        // trim_end=7.0s means chars covering 7..10s are cut
        let mask = chunk_cut_mask(10.0, 0.0, 7.0, 30.0, 30);
        assert_eq!(mask.len(), 10);
        assert!(!mask[0] && !mask[5] && !mask[6]);
        assert!(mask[7] && mask[8] && mask[9]);
    }

    #[test]
    fn cut_mask_both_leading_and_trailing() {
        // 10s, trim 0..3s and 7..10s
        let mask = chunk_cut_mask(10.0, 3.0, 7.0, 30.0, 30);
        assert_eq!(mask.len(), 10);
        assert!(mask[0] && mask[1] && mask[2]);
        assert!(!mask[3] && !mask[4] && !mask[5] && !mask[6]);
        assert!(mask[7] && mask[8] && mask[9]);
    }

    #[test]
    fn cut_mask_single_char_with_trim() {
        let mask = chunk_cut_mask(0.5, 0.0, 0.3, 30.0, 30);
        assert_eq!(mask.len(), 1);
        assert!(mask[0]);
    }

    #[test]
    fn cut_mask_high_zoom_frames_per_char_1() {
        // 1s at 30fps, frames_per_char=1 => 30 chars
        // trim_start=0.5s means first 15 chars are cut
        let mask = chunk_cut_mask(1.0, 0.5, 1.0, 30.0, 1);
        assert_eq!(mask.len(), 30);
        for i in 0..15 {
            assert!(mask[i], "char {} should be cut", i);
        }
        for i in 15..30 {
            assert!(!mask[i], "char {} should not be cut", i);
        }
    }

    #[test]
    fn cut_mask_tiny_trim_treated_as_no_trim() {
        let mask = chunk_cut_mask(10.0, 0.005, 10.0, 30.0, 30);
        assert!(mask.iter().all(|&c| !c));
    }
}
