use crate::config::QrecConfig;
use crate::timeline;

const FPS: f64 = 30.0;

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusRegion {
    Controls,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlsRow {
    Screen,
    Microphone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Ready,
    Recording,
    Rendering,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Tab,
    Esc,
    Enter,
}

#[derive(Debug, Clone)]
pub struct App {
    pub config: QrecConfig,
    pub state: AppState,
    pub focus: FocusRegion,
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
    pub pending_quit: bool,
    pub pending_overwrite: bool,
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
    DiscardLastChunk,
    CheckOverwriteThenRender {
        files: Vec<String>,
        output: String,
    },
    Render {
        files: Vec<String>,
        output: String,
    },
    PreviewChunk(usize),
    PreviewAll,
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
}

impl App {
    pub fn new(config: QrecConfig, screens: Vec<ScreenInfo>, microphones: Vec<String>) -> Self {
        let frames_per_char = if config.chunks.is_empty() {
            1
        } else {
            timeline::default_frames_per_char(&config.chunks, FPS, 80)
        };

        Self {
            config,
            state: AppState::Ready,
            focus: FocusRegion::Controls,
            controls_row: ControlsRow::Screen,
            selected_chunk: 0,
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
            pending_quit: false,
            pending_overwrite: false,
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

    if new.pending_quit {
        match key {
            Key::Char('y') | Key::Char('Y') => {
                new.pending_quit = false;
                if new.state == AppState::Recording {
                    commands.push(AppCommand::StopRecording);
                }
                new.state = AppState::Exited;
            }
            Key::Char('n') | Key::Char('N') | Key::Esc => {
                new.pending_quit = false;
                new.status_message = "Ready".to_string();
            }
            _ => {}
        }
        return (new, commands);
    }

    if new.pending_overwrite {
        match key {
            Key::Char('y') | Key::Char('Y') => {
                new.pending_overwrite = false;
                let files: Vec<String> = new.config.chunks.iter().map(|c| c.file.clone()).collect();
                commands.push(AppCommand::Render {
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
            new.pending_quit = true;
            new.status_message = "Quit? (y/n)".to_string();
        }
        Key::Char('r') => match new.state {
            AppState::Recording => {
                commands.push(AppCommand::StopRecording);
            }
            AppState::Rendering => {}
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
        Key::Tab => {
            new.focus = match new.focus {
                FocusRegion::Controls => FocusRegion::Timeline,
                FocusRegion::Timeline => FocusRegion::Controls,
            };
        }
        _ => match new.state {
            AppState::Recording | AppState::Rendering | AppState::Exited => {}
            AppState::Ready => match new.focus {
                FocusRegion::Controls => {
                    handle_controls_key(&mut new, key, &mut commands);
                }
                FocusRegion::Timeline => {
                    handle_timeline_key(&mut new, key, &mut commands);
                }
            },
        },
    }

    (new, commands)
}

fn handle_controls_key(app: &mut App, key: Key, commands: &mut Vec<AppCommand>) {
    match key {
        Key::Char('j') | Key::Down => {
            app.controls_row = match app.controls_row {
                ControlsRow::Screen => ControlsRow::Microphone,
                ControlsRow::Microphone => ControlsRow::Screen,
            };
        }
        Key::Char('k') | Key::Up => {
            app.controls_row = match app.controls_row {
                ControlsRow::Screen => ControlsRow::Microphone,
                ControlsRow::Microphone => ControlsRow::Screen,
            };
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
        },
        Key::Char('d') => {
            if !app.config.chunks.is_empty() {
                commands.push(AppCommand::DiscardLastChunk);
            } else {
                app.status_message = "No chunk to remove".to_string();
            }
        }
        Key::Char('e') => {
            let files: Vec<String> = app.config.chunks.iter().map(|c| c.file.clone()).collect();
            commands.push(AppCommand::CheckOverwriteThenRender {
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

fn handle_timeline_key(app: &mut App, key: Key, commands: &mut Vec<AppCommand>) {
    if app.config.chunks.is_empty() {
        match key {
            Key::Char('e') => {
                let files: Vec<String> = app.config.chunks.iter().map(|c| c.file.clone()).collect();
                commands.push(AppCommand::CheckOverwriteThenRender {
                    files,
                    output: "output.mp4".to_string(),
                });
            }
            Key::Char('P') => {
                commands.push(AppCommand::PreviewAll);
            }
            _ => {}
        }
        return;
    }

    match key {
        Key::Char('h') | Key::Left => {
            if app.selected_chunk > 0 {
                app.selected_chunk -= 1;
            }
            auto_pan_to_selected(app);
            update_chunk_status(app);
        }
        Key::Char('l') | Key::Right => {
            if app.selected_chunk + 1 < app.config.chunks.len() {
                app.selected_chunk += 1;
            }
            auto_pan_to_selected(app);
            update_chunk_status(app);
        }
        Key::Char('H') => {
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
        Key::Char('L') => {
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
        Key::Char('i') => {
            app.frames_per_char = timeline::zoom_in(app.frames_per_char);
            auto_pan_to_selected(app);
        }
        Key::Char('o') => {
            app.frames_per_char = timeline::zoom_out(app.frames_per_char);
            auto_pan_to_selected(app);
        }
        Key::Char('d') => {
            commands.push(AppCommand::DeleteChunk(app.selected_chunk));
        }
        Key::Char('p') => {
            commands.push(AppCommand::PreviewChunk(app.selected_chunk));
        }
        Key::Char('e') => {
            let files: Vec<String> = app.config.chunks.iter().map(|c| c.file.clone()).collect();
            commands.push(AppCommand::CheckOverwriteThenRender {
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

pub fn apply_event(app: &App, event: AppEvent) -> App {
    let mut new = app.clone();

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
            fix_selected_chunk(&mut new);
            recalc_zoom(&mut new);
            auto_pan_to_selected(&mut new);
        }
        AppEvent::FileDeleteFailed(_idx, reason) => {
            new.status_message = format!("Delete failed: {}", reason);
        }
        AppEvent::RenderSucceeded(output) => {
            new.state = AppState::Ready;
            new.status_message = format!("Rendered {}", output);
        }
        AppEvent::RenderFailed(msg) => {
            new.state = AppState::Ready;
            new.status_message = format!("Render error: {}", msg);
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
                new.status_message = "No chunks to render".to_string();
            } else {
                new.state = AppState::Rendering;
                new.status_message = "Rendering...".to_string();
            }
        }
        AppEvent::PreviewDone(msg) => {
            new.status_message = msg;
        }
    }

    new
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
