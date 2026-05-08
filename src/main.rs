mod app;
mod command;
mod config;
mod effects;
mod timeline;
mod ui;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, AppAction, AppState, Key};

fn main() -> anyhow::Result<()> {
    let missing = effects::check_dependencies();
    if !missing.is_empty() {
        eprintln!("Missing dependencies. Please install them:");
        for dep in &missing {
            eprintln!("  - {}", dep);
        }
        std::process::exit(1);
    }

    let config = effects::load_config()?;

    let screens = effects::discover_screens().unwrap_or_else(|e| {
        eprintln!("Warning: could not discover screens: {}", e);
        vec![]
    });

    let microphones = effects::discover_microphones().unwrap_or_else(|e| {
        eprintln!("Warning: could not discover microphones: {}", e);
        vec!["No microphone".to_string()]
    });

    let screen_index = config
        .selected_screen
        .as_ref()
        .and_then(|s| screens.iter().position(|sc| &sc.name == s))
        .unwrap_or(0);

    let mic_index = config
        .selected_microphone
        .as_ref()
        .and_then(|m| microphones.iter().position(|mic| mic == m))
        .unwrap_or(0);

    let mut app = App::new(config.clone(), screens, microphones);
    app.screen_index = screen_index;
    app.mic_index = mic_index;

    effects::ensure_config_exists(&app.config)?;

    enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen)?;

    let mut recorder_child: Option<std::process::Child> = None;

    let result = run_app(&mut terminal, &mut app, &mut recorder_child);

    if let Some(ref mut child) = recorder_child {
        let _ = effects::stop_recording(child);
    }

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    recorder_child: &mut Option<std::process::Child>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let viewport_width = terminal.size()?.width.saturating_sub(2) as usize;
        app.viewport_width = viewport_width;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let key = translate_key(key);
                let actions = app.handle_key(key);

                for action in actions {
                    match action {
                        AppAction::Quit => {
                            return Ok(());
                        }
                        AppAction::SaveConfig => {
                            if let Err(e) = effects::save_config(&app.config) {
                                app.status_message = format!("Error: {}", e);
                            }
                        }
                        AppAction::StartRecording => {
                            start_recording(app, recorder_child);
                        }
                        AppAction::StopRecording => {
                            stop_recording(app, recorder_child);
                        }
                        AppAction::DeleteChunk(idx) => {
                            delete_chunk(app, idx);
                        }
                        AppAction::DiscardLastChunk => {
                            discard_last_chunk(app);
                        }
                        AppAction::RequestRender => {
                            request_render(app);
                        }
                        AppAction::Render => {
                            do_render(app);
                        }
                        AppAction::PreviewChunk(idx) => {
                            preview_chunk(app, idx, terminal);
                        }
                        AppAction::PreviewAll => {
                            preview_all(app, terminal);
                        }
                    }
                }
            }
        }
    }
}

fn start_recording(app: &mut App, recorder_child: &mut Option<std::process::Child>) {
    let screen = match app.selected_screen() {
        Some(s) => s.to_string(),
        None => {
            app.status_message = "No screen selected".to_string();
            return;
        }
    };

    let filename = app.config.generate_chunk_filename();
    let audio = app.selected_microphone().map(|s| s.to_string());

    app.config.next_chunk_number += 1;
    let _ = effects::save_config(&app.config);

    match effects::start_recording(&screen, audio.as_deref(), &filename) {
        Ok(child) => {
            *recorder_child = Some(child);
            app.state = AppState::Recording;
            app.recording_chunk_file = Some(filename);
            app.recording_start_time = Some(std::time::Instant::now());
            app.status_message = "Recording...".to_string();
        }
        Err(e) => {
            app.config.next_chunk_number -= 1;
            app.status_message = format!("Error starting recording: {}", e);
        }
    }
}

fn stop_recording(app: &mut App, recorder_child: &mut Option<std::process::Child>) {
    if let Some(ref mut child) = recorder_child {
        if let Err(e) = effects::stop_recording(child) {
            app.status_message = format!("Error stopping recording: {}", e);
        }
        *recorder_child = None;
    }

    let chunk_file = app.recording_chunk_file.take().unwrap_or_default();

    if !effects::file_exists(&chunk_file) {
        app.status_message = format!(
            "Recording failed: {} was not created by wf-recorder. Check logs.txt",
            chunk_file
        );
        app.state = AppState::Ready;
        app.recording_start_time = None;
        return;
    }

    let duration = effects::get_video_duration(&chunk_file).unwrap_or(0.0);

    timeline::add_chunk(&mut app.config, chunk_file.clone(), duration);

    if let Err(e) = effects::save_config(&app.config) {
        app.status_message = format!("Error saving config: {}", e);
    } else {
        app.status_message = format!("Recorded {}", chunk_file);
    }

    app.state = AppState::Ready;
    app.recording_start_time = None;
    app.recalc_zoom();

    if !app.config.chunks.is_empty() {
        app.selected_chunk = app.config.chunks.len() - 1;
        app.auto_pan_to_selected();
    }
}

fn delete_chunk(app: &mut App, idx: usize) {
    if let Some(file) = timeline::remove_chunk(&mut app.config, idx) {
        let _ = effects::delete_file(&file);
        let _ = effects::save_config(&app.config);
        app.status_message = format!("Deleted {}", file);

        if !app.config.chunks.is_empty() {
            if app.selected_chunk >= app.config.chunks.len() {
                app.selected_chunk = app.config.chunks.len() - 1;
            }
        } else {
            app.selected_chunk = 0;
        }
        app.recalc_zoom();
        app.auto_pan_to_selected();
    } else {
        app.status_message = "No chunk to remove".to_string();
    }
}

fn discard_last_chunk(app: &mut App) {
    if app.config.chunks.is_empty() {
        app.status_message = "No chunk to remove".to_string();
        return;
    }
    let last_idx = app.config.chunks.len() - 1;
    delete_chunk(app, last_idx);
}

fn request_render(app: &mut App) {
    if app.config.chunks.is_empty() {
        app.status_message = "No chunks to render".to_string();
        return;
    }
    if effects::file_exists("output.mp4") {
        app.pending_overwrite = true;
        app.status_message = "output.mp4 exists. Overwrite? (y/n)".to_string();
    } else {
        do_render(app);
    }
}

fn do_render(app: &mut App) {
    if app.config.chunks.is_empty() {
        app.status_message = "No chunks to render".to_string();
        return;
    }

    app.state = AppState::Rendering;
    app.status_message = "Rendering...".to_string();

    let files: Vec<String> = app.config.chunks.iter().map(|c| c.file.clone()).collect();
    match effects::render_concat(&files, "output.mp4") {
        Ok(()) => {
            app.status_message = "Rendered output.mp4".to_string();
        }
        Err(e) => {
            app.status_message = format!("Render error: {}", e);
        }
    }
    app.state = AppState::Ready;
}

fn preview_chunk(app: &mut App, idx: usize, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    if app.state == AppState::Recording {
        return;
    }
    if let Some(chunk) = app.config.chunks.get(idx) {
        if !std::path::Path::new(&chunk.file).exists() {
            app.status_message = format!("File not found: {}", chunk.file);
            return;
        }
        suspend_terminal_and(terminal, || effects::preview_file(&chunk.file));
        app.status_message = format!("Previewed {}", chunk.file);
    }
}

fn preview_all(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    if app.state == AppState::Recording {
        return;
    }
    if app.config.chunks.is_empty() {
        app.status_message = "No chunks to preview".to_string();
        return;
    }
    let files: Vec<String> = app
        .config
        .chunks
        .iter()
        .map(|c| c.file.clone())
        .filter(|f| std::path::Path::new(f).exists())
        .collect();
    if files.is_empty() {
        app.status_message = "No chunk files found on disk".to_string();
        return;
    }
    suspend_terminal_and(terminal, || effects::preview_files(&files));
    app.status_message = "Previewed all chunks".to_string();
}

fn suspend_terminal_and<F: FnOnce() -> anyhow::Result<()>>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    f: F,
) {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen);

    if let Err(e) = f() {
        eprintln!("Error: {}", e);
    }

    let _ = crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen);
    let _ = enable_raw_mode();
    let _ = terminal.clear();
}

fn translate_key(key: crossterm::event::KeyEvent) -> Key {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        _ => Key::Esc,
    }
}
