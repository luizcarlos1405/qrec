mod app;
mod command;
mod config;
mod effects;
mod timeline;
mod ui;

use std::io;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, AppCommand, AppEvent, AppState, Key};

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

    if app.config.autotrim_enabled && !app.config.chunks.is_empty() {
        app.trim_cache_epoch += 1;
    }

    effects::ensure_config_exists(&app.config)?;

    enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen)?;

    let mut recorder_child: Option<std::process::Child> = None;
    let mut recording_start: Option<std::time::Instant> = None;

    let result = run_app(
        &mut terminal,
        &mut app,
        &mut recorder_child,
        &mut recording_start,
    );

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
    recording_start: &mut Option<std::time::Instant>,
) -> anyhow::Result<()> {
    let mut trim_rx: Option<mpsc::Receiver<(u64, String, f64, f64)>> = None;
    let mut last_trim_epoch: u64 = 0;

    loop {
        if let Some(ref rx) = trim_rx {
            while let Ok((epoch, chunk_id, trim_start, trim_end)) = rx.try_recv() {
                let event = AppEvent::TrimCacheEntry {
                    epoch,
                    chunk_id,
                    trim_start,
                    trim_end,
                };
                *app = app::apply_event(app, event);
            }
        }

        if app.state == AppState::Recording {
            app.recording_elapsed_secs = recording_start
                .map(|t| t.elapsed().as_secs_f64())
                .unwrap_or(0.0);
        }

        terminal.draw(|f| ui::draw(f, app))?;

        let viewport_width = terminal.size()?.width.saturating_sub(2) as usize;
        app.viewport_width = viewport_width;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key_event) = event::read()? {
                let key = translate_key(key_event);
                let (new_app, commands) = app::handle_key(app, key);
                *app = new_app;

                for cmd in commands {
                    let events =
                        execute_command(cmd, app, recorder_child, recording_start, terminal);
                    for ev in events {
                        *app = app::apply_event(app, ev);
                    }
                }

                if app.state == AppState::Exited {
                    return Ok(());
                }
            }
        }

        if app.trim_cache_epoch != last_trim_epoch
            && app.config.autotrim_enabled
            && !app.config.chunks.is_empty()
        {
            last_trim_epoch = app.trim_cache_epoch;
            trim_rx = Some(spawn_trim_computation(
                &app.config.chunks,
                app.config.autotrim_threshold_db,
                app.trim_cache_epoch,
            ));
        }
    }
}

fn spawn_trim_computation(
    chunks: &[config::Chunk],
    threshold_db: f64,
    epoch: u64,
) -> mpsc::Receiver<(u64, String, f64, f64)> {
    let (tx, rx) = mpsc::channel();
    let chunks: Vec<_> = chunks.to_vec();
    std::thread::spawn(move || {
        for chunk in chunks {
            if !std::path::Path::new(&chunk.file).exists() {
                continue;
            }
            let duration = match effects::get_video_duration(&chunk.file) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let silence = match effects::detect_silence(&chunk.file, threshold_db) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (trim_start, trim_end) = effects::compute_trim_points(duration, &silence);
            if tx
                .send((epoch, chunk.id.clone(), trim_start, trim_end))
                .is_err()
            {
                break;
            }
        }
    });
    rx
}

fn execute_command(
    cmd: AppCommand,
    app: &mut App,
    recorder_child: &mut Option<std::process::Child>,
    recording_start: &mut Option<std::time::Instant>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Vec<AppEvent> {
    match cmd {
        AppCommand::SaveConfig => {
            let config_to_save = app.config.clone();
            match effects::save_config(&config_to_save) {
                Ok(()) => vec![AppEvent::ConfigSaved],
                Err(e) => vec![AppEvent::ConfigSaveFailed(e.to_string())],
            }
        }
        AppCommand::StartRecording {
            screen,
            mic,
            filename,
        } => {
            let config_to_save = app.config.clone();
            let _ = effects::save_config(&config_to_save);
            match effects::start_recording(&screen, mic.as_deref(), &filename) {
                Ok(child) => {
                    *recorder_child = Some(child);
                    *recording_start = Some(std::time::Instant::now());
                    vec![AppEvent::RecordingStarted { filename }]
                }
                Err(e) => {
                    vec![AppEvent::RecordingFailed {
                        filename,
                        reason: e.to_string(),
                    }]
                }
            }
        }
        AppCommand::StopRecording => {
            if let Some(ref mut child) = recorder_child {
                let _ = effects::stop_recording(child);
                *recorder_child = None;
            }
            let chunk_file = app.recording_chunk_file.clone().unwrap_or_default();
            if !effects::file_exists(&chunk_file) {
                *recording_start = None;
                return vec![AppEvent::RecordingFileMissing { chunk_file }];
            }
            let duration = effects::get_video_duration(&chunk_file).unwrap_or(0.0);
            *recording_start = None;
            vec![AppEvent::RecordingStopped {
                chunk_file,
                duration_secs: duration,
            }]
        }
        AppCommand::DeleteChunk(idx) => {
            let (new_config, removed_file) = timeline::remove_chunk(app.config.clone(), idx);
            app.config = new_config;
            match removed_file {
                Some(file) => {
                    let _ = effects::delete_file(&file);
                    let _ = effects::save_config(&app.config);
                    vec![AppEvent::FileDeleted(file)]
                }
                None => vec![AppEvent::FileDeleteFailed(
                    idx,
                    "No chunk to remove".to_string(),
                )],
            }
        }
        AppCommand::DiscardLastChunk => {
            if app.config.chunks.is_empty() {
                return vec![AppEvent::FileDeleteFailed(
                    0,
                    "No chunk to remove".to_string(),
                )];
            }
            let last_idx = app.config.chunks.len() - 1;
            let (new_config, removed_file) = timeline::remove_chunk(app.config.clone(), last_idx);
            app.config = new_config;
            match removed_file {
                Some(file) => {
                    let _ = effects::delete_file(&file);
                    let _ = effects::save_config(&app.config);
                    vec![AppEvent::FileDeleted(file)]
                }
                None => vec![AppEvent::FileDeleteFailed(
                    last_idx,
                    "No chunk to remove".to_string(),
                )],
            }
        }
        AppCommand::CheckOverwriteThenRender { files, output } => {
            if files.is_empty() {
                return vec![AppEvent::OverwriteCheckResult {
                    exists: false,
                    files,
                    output,
                }];
            }
            let exists = effects::file_exists(&output);
            if exists {
                vec![AppEvent::OverwriteCheckResult {
                    exists: true,
                    files,
                    output,
                }]
            } else {
                app.state = AppState::Rendering;
                app.status_message = "Rendering...".to_string();
                let result = if app.config.autotrim_enabled {
                    effects::render_concat_autotrim(
                        &files,
                        &output,
                        app.config.autotrim_threshold_db,
                    )
                } else {
                    effects::render_concat(&files, &output)
                };
                match result {
                    Ok(()) => vec![AppEvent::RenderSucceeded(output)],
                    Err(e) => vec![AppEvent::RenderFailed(e.to_string())],
                }
            }
        }
        AppCommand::Render { files, output } => {
            app.state = AppState::Rendering;
            app.status_message = "Rendering...".to_string();
            let result = if app.config.autotrim_enabled {
                effects::render_concat_autotrim(&files, &output, app.config.autotrim_threshold_db)
            } else {
                effects::render_concat(&files, &output)
            };
            match result {
                Ok(()) => vec![AppEvent::RenderSucceeded(output)],
                Err(e) => vec![AppEvent::RenderFailed(e.to_string())],
            }
        }
        AppCommand::PreviewChunk(idx) => {
            if app.state == AppState::Recording {
                return vec![];
            }
            if let Some(chunk) = app.config.chunks.get(idx) {
                if !std::path::Path::new(&chunk.file).exists() {
                    return vec![AppEvent::PreviewDone(format!(
                        "File not found: {}",
                        chunk.file
                    ))];
                }
                let file = chunk.file.clone();
                if app.config.autotrim_enabled {
                    let threshold = app.config.autotrim_threshold_db;
                    suspend_terminal_and(terminal, || {
                        effects::preview_file_autotrim(&file, threshold)
                    });
                } else {
                    suspend_terminal_and(terminal, || effects::preview_file(&file));
                }
                vec![AppEvent::PreviewDone(format!("Previewed {}", chunk.file))]
            } else {
                vec![]
            }
        }
        AppCommand::PreviewAll => {
            if app.state == AppState::Recording {
                return vec![];
            }
            if app.config.chunks.is_empty() {
                return vec![AppEvent::PreviewDone("No chunks to preview".to_string())];
            }
            let files: Vec<String> = app
                .config
                .chunks
                .iter()
                .map(|c| c.file.clone())
                .filter(|f| std::path::Path::new(f).exists())
                .collect();
            if files.is_empty() {
                return vec![AppEvent::PreviewDone(
                    "No chunk files found on disk".to_string(),
                )];
            }
            if app.config.autotrim_enabled {
                let threshold = app.config.autotrim_threshold_db;
                suspend_terminal_and(terminal, || {
                    effects::preview_files_autotrim(&files, threshold)
                });
            } else {
                suspend_terminal_and(terminal, || effects::preview_files(&files));
            }
            vec![AppEvent::PreviewDone("Previewed all chunks".to_string())]
        }
        AppCommand::RefreshTrimCache => vec![],
    }
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
