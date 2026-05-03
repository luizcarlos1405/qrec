use crate::config::{Chunk, QrecConfig};
use chrono::Utc;

pub fn add_chunk(config: &mut QrecConfig, file: String, duration_secs: f64) {
    let id = config.generate_chunk_id();
    let chunk = Chunk {
        id,
        file,
        duration_secs,
        recorded_at: Utc::now(),
    };
    config.chunks.push(chunk);
    config.next_chunk_number += 1;
}

pub fn remove_chunk(config: &mut QrecConfig, index: usize) -> Option<String> {
    if index < config.chunks.len() {
        let chunk = config.chunks.remove(index);
        Some(chunk.file.clone())
    } else {
        None
    }
}

pub fn move_chunk_left(config: &mut QrecConfig, index: usize) -> bool {
    if index == 0 || index >= config.chunks.len() {
        return false;
    }
    config.chunks.swap(index, index - 1);
    true
}

pub fn move_chunk_right(config: &mut QrecConfig, index: usize) -> bool {
    if index + 1 >= config.chunks.len() {
        return false;
    }
    config.chunks.swap(index, index + 1);
    true
}

pub fn chunk_char_width(duration_secs: f64, fps: f64, frames_per_char: usize) -> usize {
    if frames_per_char == 0 {
        return 1;
    }
    let frames = duration_secs * fps;
    (frames / frames_per_char as f64).ceil() as usize
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


