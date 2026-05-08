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

pub fn detect_silence(file: &str, threshold_db: f64) -> anyhow::Result<Vec<(f64, Option<f64>)>> {
    let cmd = command::ffmpeg_silence_detect_command(file, threshold_db);
    let output = match Command::new(&cmd.program)
        .args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };

    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        log_error(&format!(
            "silencedetect failed for '{}': {}",
            file,
            stderr.trim()
        ));
        return Ok(Vec::new());
    }

    let mut starts = Vec::new();
    let mut ends = Vec::new();

    for line in stderr.lines() {
        if let Some(rest) = line.split("silence_start: ").nth(1) {
            if let Ok(t) = rest.trim().parse::<f64>() {
                starts.push(t);
            }
        }
        if let Some(rest) = line.split("silence_end: ").nth(1) {
            let end_str = rest.split_whitespace().next().unwrap_or("0");
            if let Ok(t) = end_str.parse::<f64>() {
                ends.push(t);
            }
        }
    }

    let intervals: Vec<(f64, Option<f64>)> = starts
        .into_iter()
        .enumerate()
        .map(|(i, s)| (s, ends.get(i).copied()))
        .collect();

    log_error(&format!(
        "detected {} silence intervals in '{}'",
        intervals.len(),
        file
    ));

    Ok(intervals)
}

pub fn compute_trim_points(duration: f64, silence_intervals: &[(f64, Option<f64>)]) -> (f64, f64) {
    let mut trim_start = 0.0;
    let mut trim_end = duration;

    if let Some(&(start, end)) = silence_intervals.first() {
        if start < 0.01 {
            if let Some(e) = end {
                trim_start = e;
            }
        }
    }

    if let Some(&(start, end)) = silence_intervals.last() {
        match end {
            Some(e) if e >= duration - 0.01 => {
                trim_end = start;
            }
            None => {
                trim_end = start;
            }
            _ => {}
        }
    }

    if trim_end <= trim_start {
        return (0.0, duration);
    }

    (trim_start, trim_end)
}

pub fn preview_file_autotrim(file: &str, threshold_db: f64) -> anyhow::Result<()> {
    let duration = get_video_duration(file)?;
    let silence = detect_silence(file, threshold_db)?;
    let (trim_start, trim_end) = compute_trim_points(duration, &silence);

    log_error(&format!(
        "autotrim preview '{}': trim {:.3} - {:.3} (of {:.3})",
        file, trim_start, trim_end, duration
    ));

    let cmd = command::mpv_preview_segment_command(file, trim_start, trim_end);
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

pub fn preview_files_autotrim(files: &[String], threshold_db: f64) -> anyhow::Result<()> {
    let mut edl_lines = Vec::new();

    for file in files {
        let duration = get_video_duration(file)?;
        let silence = detect_silence(file, threshold_db)?;
        let (trim_start, trim_end) = compute_trim_points(duration, &silence);

        let abs_path = std::path::Path::new(file)
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| file.clone());

        let length = trim_end - trim_start;
        if length > 0.0 {
            edl_lines.push(format!("{},{},{}", abs_path, trim_start, length));
        }
    }

    if edl_lines.is_empty() {
        anyhow::bail!("No content after silence removal");
    }

    let edl_content = format!("# mpv EDL v0\n{}", edl_lines.join("\n"));
    let mut temp_file = tempfile::Builder::new().suffix(".edl").tempfile()?;
    write!(temp_file, "{}", edl_content)?;
    temp_file.flush()?;

    let edl_path = temp_file.path().to_string_lossy().to_string();
    log_error(&format!(
        "autotrim preview EDL: {}",
        edl_content.replace('\n', " | ")
    ));

    let status = match Command::new("mpv").arg(&edl_path).status() {
        Ok(s) => s,
        Err(e) => {
            log_error(&format!("Failed to launch mpv: {}", e));
            return Err(e.into());
        }
    };

    drop(temp_file);

    if !status.success() {
        let msg = format!("mpv exited with status {} for EDL preview", status);
        log_error(&msg);
        anyhow::bail!("{}", msg);
    }
    Ok(())
}

const RAW_RENDER_TEMP: &str = "qrec-render-raw.mp4";

pub fn render_concat_autotrim(
    chunk_files: &[String],
    output_file: &str,
    threshold_db: f64,
) -> anyhow::Result<()> {
    render_concat(chunk_files, RAW_RENDER_TEMP)?;

    let duration = get_video_duration(RAW_RENDER_TEMP)?;
    let silence = detect_silence(RAW_RENDER_TEMP, threshold_db)?;
    let (trim_start, trim_end) = compute_trim_points(duration, &silence);

    log_error(&format!(
        "autotrim render: trim {:.3} - {:.3} (of {:.3})",
        trim_start, trim_end, duration
    ));

    let needs_trim = (trim_start - 0.0).abs() > 0.01 || (trim_end - duration).abs() > 0.01;

    if needs_trim {
        let cmd = command::ffmpeg_trim_command(RAW_RENDER_TEMP, output_file, trim_start, trim_end);
        let output = Command::new(&cmd.program)
            .args(&cmd.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        let _ = std::fs::remove_file(RAW_RENDER_TEMP);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = format!(
                "ffmpeg trim failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
            log_error(&msg);
            anyhow::bail!("{}", msg);
        }
    } else {
        std::fs::rename(RAW_RENDER_TEMP, output_file)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_no_silence() {
        let (start, end) = compute_trim_points(30.0, &[]);
        assert_eq!(start, 0.0);
        assert_eq!(end, 30.0);
    }

    #[test]
    fn trim_leading_silence() {
        let intervals = vec![(0.0, Some(3.2)), (15.0, Some(16.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 3.2).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }

    #[test]
    fn trim_trailing_silence() {
        let intervals = vec![(10.0, Some(11.0)), (27.0, Some(30.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 0.0).abs() < 0.001);
        assert!((end - 27.0).abs() < 0.001);
    }

    #[test]
    fn trim_both_leading_and_trailing() {
        let intervals = vec![(0.0, Some(3.2)), (27.0, Some(30.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 3.2).abs() < 0.001);
        assert!((end - 27.0).abs() < 0.001);
    }

    #[test]
    fn trim_trailing_silence_without_end() {
        let intervals: Vec<(f64, Option<f64>)> = vec![(0.0, Some(3.2)), (27.0, None)];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 3.2).abs() < 0.001);
        assert!((end - 27.0).abs() < 0.001);
    }

    #[test]
    fn trim_middle_silence_only_does_not_trim() {
        let intervals = vec![(10.0, Some(12.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 0.0).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }

    #[test]
    fn trim_entire_file_silent_falls_back() {
        let intervals = vec![(0.0, Some(30.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 0.0).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }

    #[test]
    fn trim_entire_file_silent_no_end_falls_back() {
        let intervals: Vec<(f64, Option<f64>)> = vec![(0.0, None)];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 0.0).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }

    #[test]
    fn trim_very_short_leading_silence() {
        let intervals = vec![(0.0, Some(0.05)), (15.0, Some(15.5))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 0.05).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }

    #[test]
    fn trim_silence_near_zero_is_treated_as_leading() {
        let intervals = vec![(0.005, Some(2.0))];
        let (start, end) = compute_trim_points(30.0, &intervals);
        assert!((start - 2.0).abs() < 0.001);
        assert!((end - 30.0).abs() < 0.001);
    }
}
