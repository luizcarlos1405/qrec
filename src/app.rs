use std::collections::HashMap;

use crate::config::QrecConfig;
use crate::timeline;

const FPS: f64 = 30.0;

pub type TrimCache = HashMap<String, (f64, f64)>;

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlsRow {
    Screen,
    Microphone,
    Autotrim,
    AutotrimThreshold,
    AudioDelay,
    Timeline,
}

fn next_controls_row(row: ControlsRow) -> ControlsRow {
    match row {
        ControlsRow::Screen => ControlsRow::Microphone,
        ControlsRow::Microphone => ControlsRow::AudioDelay,
        ControlsRow::AudioDelay => ControlsRow::Autotrim,
        ControlsRow::Autotrim => ControlsRow::AutotrimThreshold,
        ControlsRow::AutotrimThreshold => ControlsRow::Timeline,
        ControlsRow::Timeline => ControlsRow::Screen,
    }
}

fn prev_controls_row(row: ControlsRow) -> ControlsRow {
    match row {
        ControlsRow::Screen => ControlsRow::Timeline,
        ControlsRow::Microphone => ControlsRow::Screen,
        ControlsRow::AudioDelay => ControlsRow::Microphone,
        ControlsRow::Autotrim => ControlsRow::AudioDelay,
        ControlsRow::AutotrimThreshold => ControlsRow::Autotrim,
        ControlsRow::Timeline => ControlsRow::AutotrimThreshold,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Ready,
    Recording,
    Exporting,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Esc,
    Enter,
}

#[derive(Debug, Clone)]
pub struct App {
    pub config: QrecConfig,
    pub state: AppState,
    pub controls_row: ControlsRow,
    pub selected_chunk: usize,
    pub frames_per_char: usize,
    pub viewport_scroll: usize,
    pub viewport_width: usize,
    pub screens: Vec<ScreenInfo>,
    pub screen_index: usize,
    pub microphones: Vec<String>,
    pub mic_index: usize,
    pub status_message: String,
    pub recording_chunk_file: Option<String>,
    pub recording_elapsed_secs: f64,
    pub pending_overwrite: bool,
    pub trim_cache: TrimCache,
    pub trim_cache_epoch: u64,
    pub log_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    SaveConfig,
    StartRecording {
        screen: String,
        mic: Option<String>,
        filename: String,
    },
    StopRecording,
    DeleteChunk(usize),
    CheckOverwriteThenExport {
        files: Vec<String>,
        output: String,
    },
    Export {
        files: Vec<String>,
        output: String,
    },
    PreviewChunk(usize),
    PreviewAll,
    RefreshTrimCache,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AppEvent {
    RecordingStarted {
        filename: String,
    },
    RecordingFailed {
        filename: String,
        reason: String,
    },
    RecordingStopped {
        chunk_file: String,
        duration_secs: f64,
    },
    RecordingFileMissing {
        chunk_file: String,
    },
    ConfigSaved,
    ConfigSaveFailed(String),
    FileDeleted(String),
    FileDeleteFailed(usize, String),
    RenderSucceeded(String),
    RenderFailed(String),
    OverwriteCheckResult {
        exists: bool,
        files: Vec<String>,
        output: String,
    },
    PreviewDone(String),
    TrimCacheEntry {
        epoch: u64,
        chunk_id: String,
        trim_start: f64,
        trim_end: f64,
    },
    TrimCacheComplete {
        epoch: u64,
    },
    LogContentUpdated {
        lines: Vec<String>,
    },
}

impl App {
    pub fn new(config: QrecConfig, screens: Vec<ScreenInfo>, microphones: Vec<String>) -> Self {
        let frames_per_char = if config.chunks.is_empty() {
            1
        } else {
            timeline::default_frames_per_char(&config.chunks, FPS, 80)
        };

        let trim_cache: HashMap<String, (f64, f64)> = config
            .chunks
            .iter()
            .filter_map(|c| {
                c.trim_start
                    .zip(c.trim_end)
                    .map(|(ts, te)| (c.id.clone(), (ts, te)))
            })
            .collect();

        Self {
            config,
            state: AppState::Ready,
            controls_row: ControlsRow::Screen,
            selected_chunk: if config.chunks.is_empty() { 0 } else { config.chunks.len() - 1 },
            frames_per_char,
            viewport_scroll: 0,
            viewport_width: 80,
            screens,
            screen_index: 0,
            microphones,
            mic_index: 0,
            status_message: "Ready".to_string(),
            recording_chunk_file: None,
            recording_elapsed_secs: 0.0,
            pending_overwrite: false,
            trim_cache,
            trim_cache_epoch: 0,
            log_lines: Vec::new(),
        }
    }

    pub fn selected_screen(&self) -> Option<&str> {
        self.screens.get(self.screen_index).map(|s| s.name.as_str())
    }

    pub fn selected_screen_display(&self) -> &str {
        self.screens
            .get(self.screen_index)
            .map(|s| s.label.as_str())
            .unwrap_or("(none)")
    }

    pub fn selected_microphone(&self) -> Option<&str> {
        if self.mic_index == 0 {
            return None;
        }
        self.microphones.get(self.mic_index).map(|s| s.as_str())
    }

    pub fn selected_microphone_display(&self) -> &str {
        self.microphones
            .get(self.mic_index)
            .map(|s| s.as_str())
            .unwrap_or("No microphone")
    }
}

pub fn handle_key(app: &App, key: Key) -> (App, Vec<AppCommand>) {
    let mut new = app.clone();
    let mut commands = Vec::new();

    if new.pending_overwrite {
        match key {
            Key::Char('y') | Key::Char('Y') => {
                new.pending_overwrite = false;
                let files: Vec<String> = new.config.chunks.iter().map(|c| c.file.clone()).collect();
                commands.push(AppCommand::Export {
                    files,
                    output: "output.mp4".to_string(),
                });
            }
            Key::Char('n') | Key::Char('N') | Key::Esc => {
                new.pending_overwrite = false;
                new.status_message = "Cancelled".to_string();
            }
            _ => {}
        }
        return (new, commands);
    }

    match key {
        Key::Char('q') => {
            if new.state == AppState::Recording {
                commands.push(AppCommand::StopRecording);
            }
            new.state = AppState::Exited;
        }
        Key::Char('r') => match new.state {
            AppState::Recording => {
                commands.push(AppCommand::StopRecording);
            }
            AppState::Exporting => {}
            AppState::Ready | AppState::Exited => {
                if let Some(screen) = new.selected_screen() {
                    let screen = screen.to_string();
                    let mic = new.selected_microphone().map(|s| s.to_string());
                    let filename = new.config.generate_chunk_filename();
                    new.config.next_chunk_number += 1;
                    commands.push(AppCommand::StartRecording {
                        screen,
                        mic,
                        filename,
                    });
                } else {
                    new.status_message = "No screen selected".to_string();
                }
            }
        },
        _ => match new.state {
            AppState::Recording | AppState::Exporting | AppState::Exited => {}
            AppState::Ready => {
                handle_controls_key(&mut new, key, &mut commands);
            }
        },
    }

    (new, commands)
}

fn handle_controls_key(app: &mut App, key: Key, commands: &mut Vec<AppCommand>) {
    match key {
        Key::Char('j') | Key::Down => {
            app.controls_row = next_controls_row(app.controls_row);
        }
        Key::Char('k') | Key::Up => {
            app.controls_row = prev_controls_row(app.controls_row);
        }
        Key::Char('h') | Key::Left => match app.controls_row {
            ControlsRow::Screen => {
                if app.screen_index > 0 {
                    app.screen_index -= 1;
                }
                app.config.selected_screen = app.selected_screen().map(|s| s.to_string());
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Microphone => {
                if app.mic_index > 0 {
                    app.mic_index -= 1;
                }
                app.config.selected_microphone = app.selected_microphone().map(|s| s.to_string());
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Autotrim => {
                app.config.autotrim_enabled = false;
                app.trim_cache.clear();
                for chunk in &mut app.config.chunks {
                    chunk.trim_start = None;
                    chunk.trim_end = None;
                }
                app.trim_cache_epoch += 1;
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::AutotrimThreshold => {
                app.config.autotrim_threshold_db =
                    (app.config.autotrim_threshold_db - 1.0).max(-60.0);
                commands.push(AppCommand::SaveConfig);
                if app.config.autotrim_enabled && !app.config.chunks.is_empty() {
                    app.trim_cache_epoch += 1;
                    commands.push(AppCommand::RefreshTrimCache);
                }
            }
            ControlsRow::AudioDelay => {
                app.config.audio_delay_secs =
                    ((app.config.audio_delay_secs - 0.01) * 100.0).round() / 100.0;
                app.config.audio_delay_secs = app.config.audio_delay_secs.max(-5.0);
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Timeline => {
                if !app.config.chunks.is_empty() {
                    if app.selected_chunk > 0 {
                        app.selected_chunk -= 1;
                    }
                    auto_pan_to_selected(app);
                    update_chunk_status(app);
                }
            }
        },
        Key::Char('l') | Key::Right => match app.controls_row {
            ControlsRow::Screen => {
                if app.screen_index + 1 < app.screens.len() {
                    app.screen_index += 1;
                }
                app.config.selected_screen = app.selected_screen().map(|s| s.to_string());
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Microphone => {
                if app.mic_index + 1 < app.microphones.len() {
                    app.mic_index += 1;
                }
                app.config.selected_microphone = app.selected_microphone().map(|s| s.to_string());
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Autotrim => {
                app.config.autotrim_enabled = true;
                commands.push(AppCommand::SaveConfig);
                if !app.config.chunks.is_empty() {
                    app.trim_cache_epoch += 1;
                    commands.push(AppCommand::RefreshTrimCache);
                }
            }
            ControlsRow::AutotrimThreshold => {
                app.config.autotrim_threshold_db =
                    (app.config.autotrim_threshold_db + 1.0).min(-5.0);
                commands.push(AppCommand::SaveConfig);
                if app.config.autotrim_enabled && !app.config.chunks.is_empty() {
                    app.trim_cache_epoch += 1;
                    commands.push(AppCommand::RefreshTrimCache);
                }
            }
            ControlsRow::AudioDelay => {
                app.config.audio_delay_secs =
                    ((app.config.audio_delay_secs + 0.01) * 100.0).round() / 100.0;
                app.config.audio_delay_secs = app.config.audio_delay_secs.min(5.0);
                commands.push(AppCommand::SaveConfig);
            }
            ControlsRow::Timeline => {
                if !app.config.chunks.is_empty() {
                    if app.selected_chunk + 1 < app.config.chunks.len() {
                        app.selected_chunk += 1;
                    }
                    auto_pan_to_selected(app);
                    update_chunk_status(app);
                }
            }
        },
        Key::Char('H') => {
            if app.controls_row == ControlsRow::Timeline && !app.config.chunks.is_empty() {
                let (new_config, moved) =
                    timeline::move_chunk_left(std::mem::take(&mut app.config), app.selected_chunk);
                app.config = new_config;
                if moved {
                    if app.selected_chunk > 0 {
                        app.selected_chunk -= 1;
                    }
                    commands.push(AppCommand::SaveConfig);
                }
                auto_pan_to_selected(app);
            }
        }
        Key::Char('L') => {
            if app.controls_row == ControlsRow::Timeline && !app.config.chunks.is_empty() {
                let (new_config, moved) =
                    timeline::move_chunk_right(std::mem::take(&mut app.config), app.selected_chunk);
                app.config = new_config;
                if moved {
                    if app.selected_chunk + 1 < app.config.chunks.len() {
                        app.selected_chunk += 1;
                    }
                    commands.push(AppCommand::SaveConfig);
                }
                auto_pan_to_selected(app);
            }
        }
        Key::Char('i') => {
            if app.controls_row == ControlsRow::Timeline {
                app.frames_per_char = timeline::zoom_in(app.frames_per_char);
                auto_pan_to_selected(app);
            }
        }
        Key::Char('o') => {
            if app.controls_row == ControlsRow::Timeline {
                app.frames_per_char = timeline::zoom_out(app.frames_per_char);
                auto_pan_to_selected(app);
            }
        }
        Key::Char('d') => {
            if app.config.chunks.is_empty() {
                app.status_message = "No chunk to delete".to_string();
            } else {
                commands.push(AppCommand::DeleteChunk(app.selected_chunk));
            }
        }
        Key::Char('p') => {
            if !app.config.chunks.is_empty() {
                commands.push(AppCommand::PreviewChunk(app.selected_chunk));
            }
        }
        Key::Char('e') => {
            let files: Vec<String> = app.config.chunks.iter().map(|c| c.file.clone()).collect();
            commands.push(AppCommand::CheckOverwriteThenExport {
                files,
                output: "output.mp4".to_string(),
            });
        }
        Key::Char('P') => {
            commands.push(AppCommand::PreviewAll);
        }
        _ => {}
    }
}

fn recalc_zoom(app: &mut App) {
    if app.config.chunks.is_empty() {
        app.frames_per_char = 1;
        return;
    }
    app.frames_per_char =
        timeline::default_frames_per_char(&app.config.chunks, FPS, app.viewport_width);
}

fn auto_pan_to_selected(app: &mut App) {
    if app.config.chunks.is_empty() {
        app.viewport_scroll = 0;
        return;
    }
    let idx = app.selected_chunk.min(app.config.chunks.len() - 1);
    app.selected_chunk = idx;

    let start = timeline::chunk_start_col(&app.config, idx, FPS, app.frames_per_char);
    let width = timeline::chunk_char_width(
        app.config.chunks[idx].duration_secs,
        FPS,
        app.frames_per_char,
    );

    if start < app.viewport_scroll {
        app.viewport_scroll = start;
    } else if start + width > app.viewport_scroll + app.viewport_width {
        app.viewport_scroll = start + width - app.viewport_width;
    }
}

fn update_chunk_status(app: &mut App) {
    if let Some(chunk) = app.config.chunks.get(app.selected_chunk) {
        let time = chunk.recorded_at.format("%H:%M:%S").to_string();
        app.status_message = format!("{} — {:.1}s — {}", chunk.file, chunk.duration_secs, time);
    }
}

pub fn apply_event(app: &App, event: AppEvent) -> (App, Vec<AppCommand>) {
    let mut new = app.clone();
    let mut commands = Vec::new();

    match event {
        AppEvent::RecordingStarted { filename } => {
            new.state = AppState::Recording;
            new.recording_chunk_file = Some(filename);
            new.recording_elapsed_secs = 0.0;
            new.status_message = "Recording...".to_string();
        }
        AppEvent::RecordingFailed { reason, .. } => {
            new.config.next_chunk_number -= 1;
            new.state = AppState::Ready;
            new.recording_chunk_file = None;
            new.status_message = format!("Error starting recording: {}", reason);
        }
        AppEvent::RecordingStopped {
            chunk_file,
            duration_secs,
        } => {
            new.config = timeline::add_chunk(new.config, chunk_file.clone(), duration_secs);
            new.state = AppState::Ready;
            new.recording_chunk_file = None;
            new.recording_elapsed_secs = 0.0;
            new.status_message = format!("Recorded {}", chunk_file);
            recalc_zoom(&mut new);
            if !new.config.chunks.is_empty() {
                new.selected_chunk = new.config.chunks.len() - 1;
                auto_pan_to_selected(&mut new);
            }
            if new.config.autotrim_enabled {
                new.trim_cache_epoch += 1;
            }
        }
        AppEvent::RecordingFileMissing { chunk_file } => {
            new.state = AppState::Ready;
            new.recording_chunk_file = None;
            new.recording_elapsed_secs = 0.0;
            new.status_message = format!(
                "Recording failed: {} was not created by wf-recorder. Check logs.txt",
                chunk_file
            );
        }
        AppEvent::ConfigSaved => {}
        AppEvent::ConfigSaveFailed(msg) => {
            new.status_message = format!("Error: {}", msg);
        }
        AppEvent::FileDeleted(file) => {
            new.status_message = format!("Deleted {}", file);
            new.trim_cache.retain(|_, _| true);
            fix_selected_chunk(&mut new);
            recalc_zoom(&mut new);
            auto_pan_to_selected(&mut new);
        }
        AppEvent::FileDeleteFailed(_idx, reason) => {
            new.status_message = format!("Delete failed: {}", reason);
        }
        AppEvent::RenderSucceeded(output) => {
            new.state = AppState::Ready;
            new.status_message = format!("Exported {}", output);
        }
        AppEvent::RenderFailed(msg) => {
            new.state = AppState::Ready;
            new.status_message = format!("Export error: {}", msg);
        }
        AppEvent::OverwriteCheckResult {
            exists,
            files,
            output: _,
        } => {
            if exists {
                new.pending_overwrite = true;
                new.status_message = "output.mp4 exists. Overwrite? (y/n)".to_string();
            } else if files.is_empty() {
                new.status_message = "No chunks to export".to_string();
            } else {
                new.state = AppState::Exporting;
                new.status_message = "Exporting...".to_string();
            }
        }
        AppEvent::PreviewDone(msg) => {
            new.status_message = msg;
        }
        AppEvent::TrimCacheEntry {
            epoch,
            chunk_id,
            trim_start,
            trim_end,
        } => {
            if epoch == new.trim_cache_epoch {
                new.trim_cache.insert(chunk_id, (trim_start, trim_end));
            }
        }
        AppEvent::TrimCacheComplete { epoch } => {
            if epoch == new.trim_cache_epoch {
                for chunk in &mut new.config.chunks {
                    if let Some(&(ts, te)) = new.trim_cache.get(&chunk.id) {
                        chunk.trim_start = Some(ts);
                        chunk.trim_end = Some(te);
                    }
                }
                commands.push(AppCommand::SaveConfig);
            }
        }
        AppEvent::LogContentUpdated { lines } => {
            new.log_lines = lines;
        }
    }

    (new, commands)
}

fn fix_selected_chunk(app: &mut App) {
    if !app.config.chunks.is_empty() {
        if app.selected_chunk >= app.config.chunks.len() {
            app.selected_chunk = app.config.chunks.len() - 1;
        }
    } else {
        app.selected_chunk = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Chunk, QrecConfig};
    use chrono::Utc;

    fn app_ready() -> App {
        let config = QrecConfig::default();
        let screens = vec![ScreenInfo {
            name: "HDMI-1".to_string(),
            label: "HDMI-1 (test)".to_string(),
        }];
        let mics = vec!["No microphone".to_string()];
        App::new(config, screens, mics)
    }

    fn app_ready_with_chunks(n: usize) -> App {
        let mut config = QrecConfig::default();
        for i in 1..=n {
            config.chunks.push(Chunk {
                id: format!("chunk-{}", i),
                file: format!("chunk-{}.mp4", i),
                duration_secs: 5.0,
                recorded_at: Utc::now(),
                trim_start: None,
                trim_end: None,
            });
        }
        config.next_chunk_number = (n + 1) as u64;
        let screens = vec![ScreenInfo {
            name: "HDMI-1".to_string(),
            label: "HDMI-1 (test)".to_string(),
        }];
        let mics = vec!["No microphone".to_string()];
        App::new(config, screens, mics)
    }

    fn app_recording() -> App {
        let mut app = app_ready();
        app.state = AppState::Recording;
        app.recording_chunk_file = Some("chunk-1.mp4".to_string());
        app
    }

    #[test]
    fn new_app_is_ready() {
        let app = app_ready();
        assert_eq!(app.state, AppState::Ready);
        assert_eq!(app.focus, FocusRegion::Controls);
        assert_eq!(app.frames_per_char, 1);
    }

    #[test]
    fn tab_toggles_focus() {
        let app = app_ready();
        let (app2, cmds) = handle_key(&app, Key::Tab);
        assert!(cmds.is_empty());
        assert_eq!(app2.focus, FocusRegion::Timeline);

        let (app3, cmds) = handle_key(&app2, Key::Tab);
        assert!(cmds.is_empty());
        assert_eq!(app3.focus, FocusRegion::Controls);
    }

    #[test]
    fn q_quits_immediately() {
        let app = app_ready();
        let (app2, cmds) = handle_key(&app, Key::Char('q'));
        assert!(cmds.is_empty());
        assert_eq!(app2.state, AppState::Exited);
    }

    #[test]
    fn q_while_recording_stops_first() {
        let app = app_recording();
        let (_app2, cmds) = handle_key(&app, Key::Char('q'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::StopRecording)));
    }

    #[test]
    fn r_when_ready_starts_recording() {
        let app = app_ready();
        let (_app2, cmds) = handle_key(&app, Key::Char('r'));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, AppCommand::StartRecording { .. })));
    }

    #[test]
    fn r_when_recording_stops() {
        let app = app_recording();
        let (_app2, cmds) = handle_key(&app, Key::Char('r'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::StopRecording)));
    }

    #[test]
    fn r_when_rendering_does_nothing() {
        let mut app = app_ready();
        app.state = AppState::Rendering;
        let (_app2, cmds) = handle_key(&app, Key::Char('r'));
        assert!(cmds.is_empty());
    }

    #[test]
    fn j_k_navigate_controls_rows() {
        let app = app_ready();
        assert_eq!(app.controls_row, ControlsRow::Screen);

        let (app2, _) = handle_key(&app, Key::Char('j'));
        assert_eq!(app2.controls_row, ControlsRow::Microphone);

        let (app3, _) = handle_key(&app2, Key::Char('j'));
        assert_eq!(app3.controls_row, ControlsRow::AudioDelay);

        let (app4, _) = handle_key(&app3, Key::Char('j'));
        assert_eq!(app4.controls_row, ControlsRow::Autotrim);

        let (app5, _) = handle_key(&app4, Key::Char('j'));
        assert_eq!(app5.controls_row, ControlsRow::AutotrimThreshold);

        let (app6, _) = handle_key(&app5, Key::Char('j'));
        assert_eq!(app6.controls_row, ControlsRow::Screen);

        let (app7, _) = handle_key(&app6, Key::Char('k'));
        assert_eq!(app7.controls_row, ControlsRow::AutotrimThreshold);

        let (app8, _) = handle_key(&app7, Key::Char('k'));
        assert_eq!(app8.controls_row, ControlsRow::Autotrim);

        let (app9, _) = handle_key(&app8, Key::Char('k'));
        assert_eq!(app9.controls_row, ControlsRow::AudioDelay);

        let (app10, _) = handle_key(&app9, Key::Char('k'));
        assert_eq!(app10.controls_row, ControlsRow::Microphone);

        let (app11, _) = handle_key(&app10, Key::Char('k'));
        assert_eq!(app11.controls_row, ControlsRow::Screen);
    }

    #[test]
    fn h_l_change_screen_index() {
        let screens = vec![
            ScreenInfo {
                name: "s1".to_string(),
                label: "Screen 1".to_string(),
            },
            ScreenInfo {
                name: "s2".to_string(),
                label: "Screen 2".to_string(),
            },
        ];
        let config = QrecConfig::default();
        let app = App::new(config, screens, vec!["No microphone".to_string()]);

        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert_eq!(app2.screen_index, 1);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));

        let (app3, _) = handle_key(&app2, Key::Char('h'));
        assert_eq!(app3.screen_index, 0);
    }

    #[test]
    fn h_at_zero_does_not_underflow() {
        let app = app_ready();
        let (app2, _) = handle_key(&app, Key::Char('h'));
        assert_eq!(app2.screen_index, 0);
    }

    #[test]
    fn l_at_max_does_not_overflow() {
        let app = app_ready();
        let (app2, _) = handle_key(&app, Key::Char('l'));
        assert_eq!(app2.screen_index, 0);
    }

    #[test]
    fn d_with_chunks_emits_discard() {
        let app = app_ready_with_chunks(2);
        let (app2, cmds) = handle_key(&app, Key::Char('d'));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, AppCommand::DiscardLastChunk)));
        let _ = app2;
    }

    #[test]
    fn d_without_chunks_shows_message() {
        let app = app_ready();
        let (app2, cmds) = handle_key(&app, Key::Char('d'));
        assert!(cmds.is_empty());
        assert_eq!(app2.status_message, "No chunk to remove");
    }

    #[test]
    fn timeline_h_l_navigate_chunks() {
        let mut app = app_ready_with_chunks(3);
        app.focus = FocusRegion::Timeline;
        app.selected_chunk = 1;

        let (app2, _) = handle_key(&app, Key::Char('l'));
        assert_eq!(app2.selected_chunk, 2);

        let (app3, _) = handle_key(&app2, Key::Char('h'));
        assert_eq!(app3.selected_chunk, 1);
    }

    #[test]
    fn timeline_reorder_chunks_with_shift_keys() {
        let mut app = app_ready_with_chunks(3);
        app.focus = FocusRegion::Timeline;
        app.selected_chunk = 1;

        let (app2, cmds) = handle_key(&app, Key::Char('H'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
        assert_eq!(app2.selected_chunk, 0);
        assert_eq!(app2.config.chunks[0].file, "chunk-2.mp4");

        let (app3, cmds) = handle_key(&app2, Key::Char('L'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
        assert_eq!(app3.selected_chunk, 1);
        assert_eq!(app3.config.chunks[1].file, "chunk-2.mp4");
    }

    #[test]
    fn timeline_i_o_zoom() {
        let mut app = app_ready_with_chunks(5);
        app.focus = FocusRegion::Timeline;
        app.frames_per_char = 4;

        let (app2, _) = handle_key(&app, Key::Char('i'));
        assert_eq!(app2.frames_per_char, 2);

        let (app3, _) = handle_key(&app2, Key::Char('o'));
        assert_eq!(app3.frames_per_char, 4);
    }

    #[test]
    fn timeline_d_emits_delete() {
        let mut app = app_ready_with_chunks(2);
        app.focus = FocusRegion::Timeline;
        app.selected_chunk = 1;

        let (_, cmds) = handle_key(&app, Key::Char('d'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::DeleteChunk(1))));
    }

    #[test]
    fn apply_event_recording_started() {
        let app = app_ready();
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::RecordingStarted {
                filename: "chunk-1.mp4".to_string(),
            },
        );
        assert_eq!(app2.state, AppState::Recording);
        assert_eq!(app2.recording_chunk_file, Some("chunk-1.mp4".to_string()));
        assert_eq!(app2.status_message, "Recording...");
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_recording_failed_rolls_back() {
        let mut app = app_ready();
        app.config.next_chunk_number = 5;
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::RecordingFailed {
                filename: "chunk-5.mp4".to_string(),
                reason: "boom".to_string(),
            },
        );
        assert_eq!(app2.state, AppState::Ready);
        assert_eq!(app2.config.next_chunk_number, 4);
        assert!(app2.status_message.contains("boom"));
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_recording_stopped_adds_chunk() {
        let app = app_recording();
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::RecordingStopped {
                chunk_file: "chunk-1.mp4".to_string(),
                duration_secs: 5.0,
            },
        );
        assert_eq!(app2.state, AppState::Ready);
        assert_eq!(app2.config.chunks.len(), 1);
        assert_eq!(app2.config.chunks[0].file, "chunk-1.mp4");
        assert_eq!(app2.selected_chunk, 0);
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_file_deleted_fixes_selected() {
        let mut app = app_ready_with_chunks(3);
        app.selected_chunk = 2;
        let (new_config, _) = crate::timeline::remove_chunk(app.config.clone(), 2);
        app.config = new_config;

        let (app2, cmds) = apply_event(&app, AppEvent::FileDeleted("chunk-3.mp4".to_string()));
        assert_eq!(app2.selected_chunk, 1);
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_render_succeeded() {
        let mut app = app_ready();
        app.state = AppState::Rendering;
        let (app2, cmds) = apply_event(&app, AppEvent::RenderSucceeded("output.mp4".to_string()));
        assert_eq!(app2.state, AppState::Ready);
        assert_eq!(app2.status_message, "Rendered output.mp4");
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_render_failed() {
        let mut app = app_ready();
        app.state = AppState::Rendering;
        let (app2, cmds) = apply_event(&app, AppEvent::RenderFailed("bad".to_string()));
        assert_eq!(app2.state, AppState::Ready);
        assert!(app2.status_message.contains("bad"));
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_overwrite_check_sets_pending() {
        let app = app_ready_with_chunks(1);
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::OverwriteCheckResult {
                exists: true,
                files: vec!["chunk-1.mp4".to_string()],
                output: "output.mp4".to_string(),
            },
        );
        assert!(app2.pending_overwrite);
        assert!(app2.status_message.contains("Overwrite?"));
        assert!(cmds.is_empty());
    }

    #[test]
    fn apply_event_overwrite_check_not_exists_sets_rendering() {
        let app = app_ready_with_chunks(1);
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::OverwriteCheckResult {
                exists: false,
                files: vec!["chunk-1.mp4".to_string()],
                output: "output.mp4".to_string(),
            },
        );
        assert!(!app2.pending_overwrite);
        assert_eq!(app2.state, AppState::Rendering);
        assert!(cmds.is_empty());
    }

    #[test]
    fn overwrite_confirm_y_emits_render() {
        let mut app = app_ready_with_chunks(2);
        app.pending_overwrite = true;
        let (_, cmds) = handle_key(&app, Key::Char('y'));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::Render { .. })));
    }

    #[test]
    fn overwrite_confirm_n_cancels() {
        let mut app = app_ready_with_chunks(2);
        app.pending_overwrite = true;
        let (app2, cmds) = handle_key(&app, Key::Char('n'));
        assert!(cmds.is_empty());
        assert!(!app2.pending_overwrite);
        assert_eq!(app2.status_message, "Cancelled");
    }

    #[test]
    fn no_screen_shows_error_on_record() {
        let config = QrecConfig::default();
        let app = App::new(config, vec![], vec!["No microphone".to_string()]);
        let (app2, cmds) = handle_key(&app, Key::Char('r'));
        assert!(cmds.is_empty());
        assert_eq!(app2.status_message, "No screen selected");
    }

    #[test]
    fn autotrim_toggle() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::Autotrim;
        assert!(!app.config.autotrim_enabled);

        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert!(app2.config.autotrim_enabled);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));

        let (app3, cmds) = handle_key(&app2, Key::Char('h'));
        assert!(!app3.config.autotrim_enabled);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
    }

    #[test]
    fn autotrim_threshold_adjusts_by_1db() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AutotrimThreshold;
        assert_eq!(app.config.autotrim_threshold_db, -40.0);

        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert_eq!(app2.config.autotrim_threshold_db, -39.0);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));

        let (app3, _) = handle_key(&app2, Key::Char('h'));
        assert_eq!(app3.config.autotrim_threshold_db, -40.0);
    }

    #[test]
    fn autotrim_threshold_clamps_at_bounds() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AutotrimThreshold;
        app.config.autotrim_threshold_db = -60.0;

        let (app2, _) = handle_key(&app, Key::Char('h'));
        assert_eq!(app2.config.autotrim_threshold_db, -60.0);

        let mut app2 = app2;
        app2.config.autotrim_threshold_db = -5.0;
        let (app3, _) = handle_key(&app2, Key::Char('l'));
        assert_eq!(app3.config.autotrim_threshold_db, -5.0);
    }

    #[test]
    fn autotrim_default_values() {
        let config = QrecConfig::default();
        assert!(!config.autotrim_enabled);
        assert_eq!(config.autotrim_threshold_db, -40.0);
        assert_eq!(config.audio_delay_secs, 0.0);
    }

    #[test]
    fn trim_cache_entry_applied_when_epoch_matches() {
        let mut app = app_ready_with_chunks(2);
        app.trim_cache_epoch = 1;
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::TrimCacheEntry {
                epoch: 1,
                chunk_id: "chunk-1".to_string(),
                trim_start: 3.0,
                trim_end: 5.0,
            },
        );
        assert_eq!(app2.trim_cache.get("chunk-1"), Some(&(3.0, 5.0)));
        assert!(cmds.is_empty());
    }

    #[test]
    fn trim_cache_entry_discarded_when_epoch_mismatches() {
        let mut app = app_ready_with_chunks(2);
        app.trim_cache_epoch = 2;
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::TrimCacheEntry {
                epoch: 1,
                chunk_id: "chunk-1".to_string(),
                trim_start: 3.0,
                trim_end: 5.0,
            },
        );
        assert!(app2.trim_cache.is_empty());
        assert!(cmds.is_empty());
    }

    #[test]
    fn autotrim_toggle_on_emits_refresh_trim_cache() {
        let mut app = app_ready_with_chunks(2);
        app.controls_row = ControlsRow::Autotrim;
        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert!(app2.config.autotrim_enabled);
        assert!(cmds
            .iter()
            .any(|c| matches!(c, AppCommand::RefreshTrimCache)));
    }

    #[test]
    fn autotrim_toggle_off_clears_cache() {
        let mut app = app_ready_with_chunks(2);
        app.config.autotrim_enabled = true;
        app.controls_row = ControlsRow::Autotrim;
        app.trim_cache.insert("chunk-1".to_string(), (0.0, 5.0));
        app.config.chunks[0].trim_start = Some(0.0);
        app.config.chunks[0].trim_end = Some(5.0);
        app.trim_cache_epoch = 1;
        let (app2, _cmds) = handle_key(&app, Key::Char('h'));
        assert!(!app2.config.autotrim_enabled);
        assert!(app2.trim_cache.is_empty());
        assert_eq!(app2.trim_cache_epoch, 2);
        assert_eq!(app2.config.chunks[0].trim_start, None);
        assert_eq!(app2.config.chunks[0].trim_end, None);
    }

    #[test]
    fn threshold_change_emits_refresh_when_autotrim_on() {
        let mut app = app_ready_with_chunks(2);
        app.config.autotrim_enabled = true;
        app.controls_row = ControlsRow::AutotrimThreshold;
        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert!(cmds
            .iter()
            .any(|c| matches!(c, AppCommand::RefreshTrimCache)));
        assert_eq!(app2.trim_cache_epoch, 1);
    }

    #[test]
    fn threshold_change_no_refresh_when_autotrim_off() {
        let mut app = app_ready_with_chunks(2);
        app.config.autotrim_enabled = false;
        app.controls_row = ControlsRow::AutotrimThreshold;
        let (_, cmds) = handle_key(&app, Key::Char('l'));
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, AppCommand::RefreshTrimCache)));
    }

    #[test]
    fn recording_stopped_increments_epoch_when_autotrim_on() {
        let mut app = app_recording();
        app.config.autotrim_enabled = true;
        app.trim_cache_epoch = 5;
        let (app2, cmds) = apply_event(
            &app,
            AppEvent::RecordingStopped {
                chunk_file: "chunk-1.mp4".to_string(),
                duration_secs: 5.0,
            },
        );
        assert_eq!(app2.trim_cache_epoch, 6);
        assert!(app2.trim_cache.is_empty());
        assert!(cmds.is_empty());
    }

    #[test]
    fn trim_cache_prepopulated_from_chunk_data() {
        let mut config = QrecConfig::default();
        config.chunks.push(Chunk {
            id: "chunk-1".to_string(),
            file: "chunk-1.mp4".to_string(),
            duration_secs: 10.0,
            recorded_at: Utc::now(),
            trim_start: Some(2.0),
            trim_end: Some(8.0),
        });
        config.chunks.push(Chunk {
            id: "chunk-2".to_string(),
            file: "chunk-2.mp4".to_string(),
            duration_secs: 5.0,
            recorded_at: Utc::now(),
            trim_start: None,
            trim_end: None,
        });
        let app = App::new(config, vec![], vec![]);
        assert_eq!(app.trim_cache.get("chunk-1"), Some(&(2.0, 8.0)));
        assert!(app.trim_cache.get("chunk-2").is_none());
    }

    #[test]
    fn trim_cache_complete_persists_to_chunks_and_saves() {
        let mut app = app_ready_with_chunks(2);
        app.config.chunks[0].trim_start = None;
        app.config.chunks[0].trim_end = None;
        app.trim_cache_epoch = 1;
        app.trim_cache.insert("chunk-1".to_string(), (1.5, 4.5));
        app.trim_cache.insert("chunk-2".to_string(), (0.0, 5.0));
        let (app2, cmds) = apply_event(&app, AppEvent::TrimCacheComplete { epoch: 1 });
        assert_eq!(app2.config.chunks[0].trim_start, Some(1.5));
        assert_eq!(app2.config.chunks[0].trim_end, Some(4.5));
        assert_eq!(app2.config.chunks[1].trim_start, Some(0.0));
        assert_eq!(app2.config.chunks[1].trim_end, Some(5.0));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
    }

    #[test]
    fn trim_cache_complete_discarded_on_epoch_mismatch() {
        let mut app = app_ready_with_chunks(1);
        app.trim_cache_epoch = 2;
        app.trim_cache.insert("chunk-1".to_string(), (1.0, 4.0));
        let (app2, cmds) = apply_event(&app, AppEvent::TrimCacheComplete { epoch: 1 });
        assert_eq!(app2.config.chunks[0].trim_start, None);
        assert!(cmds.is_empty());
    }

    #[test]
    fn audio_delay_default_is_zero() {
        let config = QrecConfig::default();
        assert!((config.audio_delay_secs - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn audio_delay_l_increases_by_0_01() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        let (app2, cmds) = handle_key(&app, Key::Char('l'));
        assert!((app2.config.audio_delay_secs - 0.01).abs() < f64::EPSILON);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
    }

    #[test]
    fn audio_delay_h_decreases_by_0_01() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        app.config.audio_delay_secs = 0.05;
        let (app2, cmds) = handle_key(&app, Key::Char('h'));
        assert!((app2.config.audio_delay_secs - 0.04).abs() < f64::EPSILON);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SaveConfig)));
    }

    #[test]
    fn audio_delay_h_goes_negative() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        let (app2, _) = handle_key(&app, Key::Char('h'));
        assert!((app2.config.audio_delay_secs - (-0.01)).abs() < f64::EPSILON);
    }

    #[test]
    fn audio_delay_clamps_at_max() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        app.config.audio_delay_secs = 5.0;
        let (app2, _) = handle_key(&app, Key::Char('l'));
        assert!((app2.config.audio_delay_secs - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn audio_delay_clamps_at_min() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        app.config.audio_delay_secs = -5.0;
        let (app2, _) = handle_key(&app, Key::Char('h'));
        assert!((app2.config.audio_delay_secs - (-5.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn audio_delay_no_floating_point_drift() {
        let mut app = app_ready();
        app.controls_row = ControlsRow::AudioDelay;
        for _ in 0..100 {
            let (app2, _) = handle_key(&app, Key::Char('l'));
            app = app2;
        }
        assert!((app.config.audio_delay_secs - 1.0).abs() < f64::EPSILON);
    }
}
