use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::app::ScreenInfo;
use crate::{command, config::QrecConfig};

const QREC_JSON: &str = "qrec.json";
const LOG_FILE: &str = "logs.txt";

const REQUIRED_DEPS: &[&str] = &["wf-recorder", "ffmpeg", "ffprobe", "mpv", "pactl"];

pub fn check_dependencies() -> Vec<String> {
    REQUIRED_DEPS
        .iter()
        .filter(|dep| which(dep).is_none())
        .map(|dep| dep.to_string())
        .collect()
}

fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(program);
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

fn extract_output_name(line: &str) -> Option<&str> {
    let line = line
        .strip_prefix(|c: char| c.is_ascii_digit())?
        .trim_start();
    let line = line.strip_prefix('.')?.trim_start();
    let after = line.strip_prefix("Name:")?.trim_start();
    let end = after.find(" Description:").unwrap_or(after.len());
    Some(&after[..end])
}

fn log_error(msg: &str) {
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let entry = format!("[{}] {}\n", timestamp, msg);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
        .and_then(|mut f| f.write_all(entry.as_bytes()));
}

pub fn load_config() -> anyhow::Result<QrecConfig> {
    if Path::new(QREC_JSON).exists() {
        let content = std::fs::read_to_string(QREC_JSON)?;
        crate::config::load_or_default(&content)
    } else {
        Ok(QrecConfig::default())
    }
}

pub fn save_config(config: &QrecConfig) -> anyhow::Result<()> {
    let content = crate::config::serialize_config(config)?;
    std::fs::write(QREC_JSON, content)?;
    Ok(())
}

pub fn ensure_config_exists(config: &QrecConfig) -> anyhow::Result<()> {
    if !Path::new(QREC_JSON).exists() {
        save_config(config)?;
    }
    Ok(())
}

pub fn discover_screens() -> anyhow::Result<Vec<ScreenInfo>> {
    let cmd = command::wf_recorder_list_command();
    let output = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let combined = format!("{}\n{}", stdout, stderr);

    let mut screens = Vec::new();
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(name) = extract_output_name(trimmed) {
            screens.push(ScreenInfo {
                name: name.to_string(),
                label: trimmed.to_string(),
            });
        }
    }

    Ok(screens)
}

pub fn discover_microphones() -> anyhow::Result<Vec<String>> {
    let mut mics = vec!["No microphone".to_string()];

    let cmd = command::pactl_list_sources_command();
    let output = match Command::new(&cmd.program)
        .args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(_) => return Ok(mics),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            mics.push(parts[1].to_string());
        }
    }

    Ok(mics)
}

pub fn start_recording(
    output: &str,
    audio_device: Option<&str>,
    filename: &str,
) -> anyhow::Result<std::process::Child> {
    let cmd = command::wf_recorder_command(output, audio_device, filename);
    log_error(&format!(
        "Starting wf-recorder: program={} args={:?} output={} audio={:?} filename={}",
        cmd.program, cmd.args, output, audio_device, filename
    ));
    let child = match Command::new(&cmd.program)
        .args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => {
            log_error(&format!("wf-recorder spawned with PID {}", c.id()));
            c
        }
        Err(e) => {
            let msg = format!("Failed to start wf-recorder: {}", e);
            log_error(&msg);
            return Err(e.into());
        }
    };
    Ok(child)
}

pub fn stop_recording(child: &mut std::process::Child) -> anyhow::Result<()> {
    let pid = child.id();
    log_error(&format!("Stopping wf-recorder PID {}", pid));
    unsafe {
        libc::kill(pid as i32, libc::SIGINT);
    }
    match child.wait() {
        Ok(status) => {
            log_error(&format!(
                "wf-recorder PID {} exited with status: {}",
                pid, status
            ));
        }
        Err(e) => {
            log_error(&format!("wf-recorder PID {} wait failed: {}", pid, e));
            return Err(e.into());
        }
    }
    Ok(())
}

pub fn get_video_duration(file: &str) -> anyhow::Result<f64> {
    let exists = Path::new(file).exists();
    log_error(&format!(
        "get_video_duration called for '{}' (exists={})",
        file, exists
    ));
    if !exists {
        log_error(&format!(
            "File '{}' does not exist, returning duration 0.0",
            file
        ));
        return Ok(0.0);
    }
    let output = match Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            file,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            let msg = format!("Failed to run ffprobe on '{}': {}", file, e);
            log_error(&msg);
            return Err(e.into());
        }
    };

    let duration_str = String::from_utf8_lossy(&output.stdout);
    let duration: f64 = duration_str.trim().parse().unwrap_or(0.0);
    if duration == 0.0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = format!(
            "ffprobe returned 0 duration for '{}': {}",
            file,
            stderr.trim()
        );
        log_error(&msg);
    }
    Ok(duration)
}

pub fn render_concat(chunk_files: &[String], output_file: &str) -> anyhow::Result<()> {
    let list_content = command::concat_list_content(chunk_files);

    let mut temp_file = tempfile::NamedTempFile::new()?;
    write!(temp_file, "{}", list_content)?;
    temp_file.flush()?;

    let list_path = temp_file.path().to_string_lossy().to_string();
    let cmd = command::ffmpeg_concat_command(chunk_files, output_file, &list_path);
    let output = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = format!(
            "ffmpeg failed (exit {}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        );
        log_error(&msg);
        anyhow::bail!("{}", msg);
    }

    Ok(())
}

pub fn preview_file(file: &str) -> anyhow::Result<()> {
    let cmd = command::mpv_preview_command(file);
    let status = match Command::new(&cmd.program).args(&cmd.args).status() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("Failed to launch mpv for '{}': {}", file, e);
            log_error(&msg);
            return Err(e.into());
        }
    };
    if !status.success() {
        let msg = format!("mpv exited with status {} for '{}'", status, file);
        log_error(&msg);
        anyhow::bail!("{}", msg);
    }
    Ok(())
}

pub fn preview_files(files: &[String]) -> anyhow::Result<()> {
    let cmd = command::mpv_preview_all_command(files);
    let status = match Command::new(&cmd.program).args(&cmd.args).status() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("Failed to launch mpv: {}", e);
            log_error(&msg);
            return Err(e.into());
        }
    };
    if !status.success() {
        let msg = format!("mpv exited with status {}", status);
        log_error(&msg);
        anyhow::bail!("{}", msg);
    }
    Ok(())
}

pub fn delete_file(file: &str) -> anyhow::Result<()> {
    if Path::new(file).exists() {
        std::fs::remove_file(file)?;
    }
    Ok(())
}

pub fn file_exists(file: &str) -> bool {
    Path::new(file).exists()
}
