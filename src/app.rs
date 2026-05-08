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

impl App {
    pub fn new(config: QrecConfig, screens: Vec<ScreenInfo>, microphones: Vec<String>) -> Self {
        let screen_index = 0;
        let mic_index = 0;

        let mut app = Self {
            config,
            state: AppState::Ready,
            focus: FocusRegion::Controls,
            controls_row: ControlsRow::Screen,
            selected_chunk: 0,
            frames_per_char: 1,
            viewport_scroll: 0,
            viewport_width: 80,
            screens,
            screen_index,
            microphones,
            mic_index,
            status_message: "Ready".to_string(),
            recording_chunk_file: None,
            recording_elapsed_secs: 0.0,
            pending_quit: false,
            pending_overwrite: false,
        };
        app.recalc_zoom();
        app
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

    pub fn recalc_zoom(&mut self) {
        if self.config.chunks.is_empty() {
            self.frames_per_char = 1;
            return;
        }
        self.frames_per_char =
            timeline::default_frames_per_char(&self.config.chunks, FPS, self.viewport_width);
    }

    pub fn auto_pan_to_selected(&mut self) {
        if self.config.chunks.is_empty() {
            self.viewport_scroll = 0;
            return;
        }
        let idx = self.selected_chunk.min(self.config.chunks.len() - 1);
        self.selected_chunk = idx;

        let start = timeline::chunk_start_col(&self.config, idx, FPS, self.frames_per_char);
        let width = timeline::chunk_char_width(
            self.config.chunks[idx].duration_secs,
            FPS,
            self.frames_per_char,
        );

        if start < self.viewport_scroll {
            self.viewport_scroll = start;
        } else if start + width > self.viewport_scroll + self.viewport_width {
            self.viewport_scroll = start + width - self.viewport_width;
        }
    }

    pub fn handle_key(&mut self, key: Key) -> Vec<AppAction> {
        let mut actions = Vec::new();

        if self.pending_quit {
            match key {
                Key::Char('y') | Key::Char('Y') => {
                    self.pending_quit = false;
                    if self.state == AppState::Recording {
                        actions.push(AppAction::StopRecording);
                    }
                    actions.push(AppAction::Quit);
                }
                Key::Char('n') | Key::Char('N') | Key::Esc => {
                    self.pending_quit = false;
                    self.status_message = "Ready".to_string();
                }
                _ => {}
            }
            return actions;
        }

        if self.pending_overwrite {
            match key {
                Key::Char('y') | Key::Char('Y') => {
                    self.pending_overwrite = false;
                    actions.push(AppAction::Render);
                }
                Key::Char('n') | Key::Char('N') | Key::Esc => {
                    self.pending_overwrite = false;
                    self.status_message = "Cancelled".to_string();
                }
                _ => {}
            }
            return actions;
        }

        match key {
            Key::Char('q') => {
                actions.push(AppAction::Quit);
            }
            Key::Char('r') => match self.state {
                AppState::Recording => {
                    actions.push(AppAction::StopRecording);
                }
                AppState::Rendering => {}
                AppState::Ready => {
                    actions.push(AppAction::StartRecording);
                }
            },
            Key::Tab => {
                self.focus = match self.focus {
                    FocusRegion::Controls => FocusRegion::Timeline,
                    FocusRegion::Timeline => FocusRegion::Controls,
                };
            }
            _ => match self.state {
                AppState::Recording => {}
                AppState::Rendering => {}
                AppState::Ready => match self.focus {
                    FocusRegion::Controls => {
                        self.handle_controls_key(key, &mut actions);
                    }
                    FocusRegion::Timeline => {
                        self.handle_timeline_key(key, &mut actions);
                    }
                },
            },
        }

        actions
    }

    fn handle_controls_key(
        &mut self,
        key: Key,
        actions: &mut Vec<AppAction>,
    ) {
        match key {
            Key::Char('j') | Key::Down => {
                self.controls_row = match self.controls_row {
                    ControlsRow::Screen => ControlsRow::Microphone,
                    ControlsRow::Microphone => ControlsRow::Screen,
                };
            }
            Key::Char('k') | Key::Up => {
                self.controls_row = match self.controls_row {
                    ControlsRow::Screen => ControlsRow::Microphone,
                    ControlsRow::Microphone => ControlsRow::Screen,
                };
            }
            Key::Char('h') | Key::Left => {
                match self.controls_row {
                    ControlsRow::Screen => {
                        if self.screen_index > 0 {
                            self.screen_index -= 1;
                        }
                        self.config.selected_screen = self.selected_screen().map(|s| s.to_string());
                        actions.push(AppAction::SaveConfig);
                    }
                    ControlsRow::Microphone => {
                        if self.mic_index > 0 {
                            self.mic_index -= 1;
                        }
                        self.config.selected_microphone =
                            self.selected_microphone().map(|s| s.to_string());
                        actions.push(AppAction::SaveConfig);
                    }
                }
            }
            Key::Char('l') | Key::Right => {
                match self.controls_row {
                    ControlsRow::Screen => {
                        if self.screen_index + 1 < self.screens.len() {
                            self.screen_index += 1;
                        }
                        self.config.selected_screen = self.selected_screen().map(|s| s.to_string());
                        actions.push(AppAction::SaveConfig);
                    }
                    ControlsRow::Microphone => {
                        if self.mic_index + 1 < self.microphones.len() {
                            self.mic_index += 1;
                        }
                        self.config.selected_microphone =
                            self.selected_microphone().map(|s| s.to_string());
                        actions.push(AppAction::SaveConfig);
                    }
                }
            }
            Key::Char('d') => {
                if !self.config.chunks.is_empty() {
                    actions.push(AppAction::DiscardLastChunk);
                } else {
                    self.status_message = "No chunk to remove".to_string();
                }
            }
            Key::Char('e') => {
                actions.push(AppAction::RequestRender);
            }
            Key::Char('P') => {
                actions.push(AppAction::PreviewAll);
            }
            _ => {}
        }
    }

    fn handle_timeline_key(
        &mut self,
        key: Key,
        actions: &mut Vec<AppAction>,
    ) {
        if self.config.chunks.is_empty() {
            match key {
                Key::Char('e') => {
                    actions.push(AppAction::RequestRender);
                }
                Key::Char('P') => {
                    actions.push(AppAction::PreviewAll);
                }
                _ => {}
            }
            return;
        }

        match key {
            Key::Char('h') | Key::Left => {
                if self.selected_chunk > 0 {
                    self.selected_chunk -= 1;
                }
                self.auto_pan_to_selected();
                self.update_chunk_status();
            }
            Key::Char('l') | Key::Right => {
                if self.selected_chunk + 1 < self.config.chunks.len() {
                    self.selected_chunk += 1;
                }
                self.auto_pan_to_selected();
                self.update_chunk_status();
            }
            Key::Char('H') => {
                let (new_config, moved) = timeline::move_chunk_left(std::mem::take(&mut self.config), self.selected_chunk);
                self.config = new_config;
                if moved {
                    if self.selected_chunk > 0 {
                        self.selected_chunk -= 1;
                    }
                    actions.push(AppAction::SaveConfig);
                }
                self.auto_pan_to_selected();
            }
            Key::Char('L') => {
                let (new_config, moved) = timeline::move_chunk_right(std::mem::take(&mut self.config), self.selected_chunk);
                self.config = new_config;
                if moved {
                    if self.selected_chunk + 1 < self.config.chunks.len() {
                        self.selected_chunk += 1;
                    }
                    actions.push(AppAction::SaveConfig);
                }
                self.auto_pan_to_selected();
            }
            Key::Char('i') => {
                self.frames_per_char = timeline::zoom_in(self.frames_per_char);
                self.auto_pan_to_selected();
            }
            Key::Char('o') => {
                self.frames_per_char = timeline::zoom_out(self.frames_per_char);
                self.auto_pan_to_selected();
            }
            Key::Char('d') => {
                actions.push(AppAction::DeleteChunk(self.selected_chunk));
            }
            Key::Char('p') => {
                actions.push(AppAction::PreviewChunk(self.selected_chunk));
            }
            Key::Char('e') => {
                actions.push(AppAction::RequestRender);
            }
            Key::Char('P') => {
                actions.push(AppAction::PreviewAll);
            }
            _ => {}
        }
    }

    fn update_chunk_status(&mut self) {
        if let Some(chunk) = self.config.chunks.get(self.selected_chunk) {
            let time = chunk.recorded_at.format("%H:%M:%S").to_string();
            self.status_message =
                format!("{} — {:.1}s — {}", chunk.file, chunk.duration_secs, time);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    Quit,
    SaveConfig,
    StartRecording,
    StopRecording,
    DeleteChunk(usize),
    DiscardLastChunk,
    RequestRender,
    Render,
    PreviewChunk(usize),
    PreviewAll,
}
