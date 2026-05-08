use crate::config::{Chunk, QrecConfig};
use chrono::Utc;

pub fn add_chunk(mut config: QrecConfig, file: String, duration_secs: f64) -> QrecConfig {
    let id = config.generate_chunk_id();
    let chunk = Chunk {
        id,
        file,
        duration_secs,
        recorded_at: Utc::now(),
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

pub fn can_zoom_out(chunks: &[Chunk], fps: f64, frames_per_char: usize) -> bool {
    !chunks
        .iter()
        .all(|c| chunk_char_width(c.duration_secs, fps, frames_per_char) == 1)
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
