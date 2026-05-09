#[derive(Debug, Clone)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

pub fn wf_recorder_command(output: &str, audio_device: Option<&str>, filename: &str) -> Command {
    let mut args = vec![
        "--output".to_string(),
        output.to_string(),
        "--file".to_string(),
        filename.to_string(),
    ];
    if let Some(audio) = audio_device {
        args.push("--audio".to_string());
        args.push(audio.to_string());
    }
    Command {
        program: "wf-recorder".to_string(),
        args,
    }
}

pub fn ffmpeg_concat_command(
    _chunk_files: &[String],
    output_file: &str,
    list_file: &str,
) -> Command {
    Command {
        program: "ffmpeg".to_string(),
        args: vec![
            "-f".to_string(),
            "concat".to_string(),
            "-safe".to_string(),
            "0".to_string(),
            "-i".to_string(),
            list_file.to_string(),
            "-c".to_string(),
            "copy".to_string(),
            output_file.to_string(),
        ],
    }
}

pub fn ffmpeg_concat_with_audio_delay_command(
    _chunk_files: &[String],
    output_file: &str,
    list_file: &str,
    audio_delay_secs: f64,
) -> Command {
    let delay_ms = (audio_delay_secs * 1000.0).round() as i64;
    let trim_secs = (-audio_delay_secs).abs();

    if audio_delay_secs >= 0.0 {
        Command {
            program: "ffmpeg".to_string(),
            args: vec![
                "-y".to_string(),
                "-f".to_string(),
                "concat".to_string(),
                "-safe".to_string(),
                "0".to_string(),
                "-i".to_string(),
                list_file.to_string(),
                "-map".to_string(),
                "0:v".to_string(),
                "-map".to_string(),
                "0:a".to_string(),
                "-c:v".to_string(),
                "copy".to_string(),
                "-af".to_string(),
                format!("adelay={}|{}", delay_ms, delay_ms),
                "-c:a".to_string(),
                "aac".to_string(),
                output_file.to_string(),
            ],
        }
    } else {
        Command {
            program: "ffmpeg".to_string(),
            args: vec![
                "-y".to_string(),
                "-f".to_string(),
                "concat".to_string(),
                "-safe".to_string(),
                "0".to_string(),
                "-i".to_string(),
                list_file.to_string(),
                "-map".to_string(),
                "0:v".to_string(),
                "-map".to_string(),
                "0:a".to_string(),
                "-c:v".to_string(),
                "copy".to_string(),
                "-af".to_string(),
                format!("atrim={},asetpts=PTS-STARTPTS", trim_secs),
                "-c:a".to_string(),
                "aac".to_string(),
                output_file.to_string(),
            ],
        }
    }
}

pub fn concat_list_content(chunk_files: &[String]) -> String {
    chunk_files
        .iter()
        .map(|f| {
            let path = std::path::Path::new(f);
            let abs = if path.is_absolute() {
                f.clone()
            } else {
                std::fs::canonicalize(f)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| f.clone())
            };
            format!("file '{}'", abs)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn mpv_preview_command(file: &str) -> Command {
    Command {
        program: "mpv".to_string(),
        args: vec![file.to_string()],
    }
}

pub fn mpv_preview_all_command(files: &[String]) -> Command {
    Command {
        program: "mpv".to_string(),
        args: files.to_vec(),
    }
}

pub fn wf_recorder_list_command() -> Command {
    Command {
        program: "wf-recorder".to_string(),
        args: vec!["-L".to_string()],
    }
}

pub fn pactl_list_sources_command() -> Command {
    Command {
        program: "pactl".to_string(),
        args: vec![
            "list".to_string(),
            "short".to_string(),
            "sources".to_string(),
        ],
    }
}

pub fn ffmpeg_silence_detect_command(file: &str, threshold_db: f64) -> Command {
    Command {
        program: "ffmpeg".to_string(),
        args: vec![
            "-i".to_string(),
            file.to_string(),
            "-af".to_string(),
            format!("silencedetect=noise={}dB:d=0.1", threshold_db),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ],
    }
}

pub fn ffmpeg_trim_command(input: &str, output: &str, start: f64, end: f64) -> Command {
    Command {
        program: "ffmpeg".to_string(),
        args: vec![
            "-y".to_string(),
            "-ss".to_string(),
            format!("{}", start),
            "-to".to_string(),
            format!("{}", end),
            "-i".to_string(),
            input.to_string(),
            "-c:v".to_string(),
            "libx264".to_string(),
            "-c:a".to_string(),
            "aac".to_string(),
            output.to_string(),
        ],
    }
}

pub fn mpv_preview_segment_command(file: &str, start: f64, end: f64) -> Command {
    Command {
        program: "mpv".to_string(),
        args: vec![
            format!("--start={}", start),
            format!("--end={}", end),
            file.to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wf_recorder_command_no_audio() {
        let cmd = wf_recorder_command("HDMI-1", None, "chunk-1.mp4");
        assert_eq!(cmd.program, "wf-recorder");
        assert_eq!(
            cmd.args,
            vec!["--output", "HDMI-1", "--file", "chunk-1.mp4"]
        );
    }

    #[test]
    fn wf_recorder_command_with_audio() {
        let cmd = wf_recorder_command("HDMI-1", Some("alsa_input.pci"), "chunk-1.mp4");
        assert!(cmd.args.contains(&"--audio".to_string()));
        assert!(cmd.args.contains(&"alsa_input.pci".to_string()));
    }

    #[test]
    fn ffmpeg_concat_command_structure() {
        let cmd = ffmpeg_concat_command(&[], "output.mp4", "/tmp/list.txt");
        assert_eq!(cmd.program, "ffmpeg");
        assert!(cmd.args.contains(&"-f".to_string()));
        assert!(cmd.args.contains(&"concat".to_string()));
        assert!(cmd.args.contains(&"/tmp/list.txt".to_string()));
        assert!(cmd.args.contains(&"output.mp4".to_string()));
    }

    #[test]
    fn concat_list_content_format() {
        let content = concat_list_content(&["a.mp4".to_string(), "b.mp4".to_string()]);
        assert_eq!(content, "file 'a.mp4'\nfile 'b.mp4'");
    }

    #[test]
    fn mpv_preview_command_single_file() {
        let cmd = mpv_preview_command("test.mp4");
        assert_eq!(cmd.program, "mpv");
        assert_eq!(cmd.args, vec!["test.mp4"]);
    }

    #[test]
    fn mpv_preview_all_command_files() {
        let cmd = super::mpv_preview_all_command(&["a.mp4".to_string(), "b.mp4".to_string()]);
        assert_eq!(cmd.args, vec!["a.mp4", "b.mp4"]);
    }

    #[test]
    fn wf_recorder_list_command_args() {
        let cmd = super::wf_recorder_list_command();
        assert_eq!(cmd.program, "wf-recorder");
        assert_eq!(cmd.args, vec!["-L"]);
    }

    #[test]
    fn pactl_list_sources_command_structure() {
        let cmd = pactl_list_sources_command();
        assert_eq!(cmd.program, "pactl");
        assert_eq!(cmd.args, vec!["list", "short", "sources"]);
    }

    #[test]
    fn ffmpeg_silence_detect_command_structure() {
        let cmd = ffmpeg_silence_detect_command("chunk-1.mp4", -40.0);
        assert_eq!(cmd.program, "ffmpeg");
        assert!(cmd.args.contains(&"-af".to_string()));
        assert!(cmd
            .args
            .iter()
            .any(|a| a.contains("silencedetect=noise=-40dB")));
        assert!(cmd.args.contains(&"-f".to_string()));
        assert!(cmd.args.contains(&"null".to_string()));
        assert!(cmd.args.contains(&"chunk-1.mp4".to_string()));
    }

    #[test]
    fn ffmpeg_trim_command_structure() {
        let cmd = ffmpeg_trim_command("in.mp4", "out.mp4", 3.2, 27.0);
        assert_eq!(cmd.program, "ffmpeg");
        assert!(cmd.args.contains(&"-y".to_string()));
        assert!(cmd.args.contains(&"-ss".to_string()));
        assert!(cmd.args.contains(&"3.2".to_string()));
        assert!(cmd.args.contains(&"-to".to_string()));
        assert!(cmd.args.contains(&"27".to_string()));
        assert!(cmd.args.contains(&"-c:v".to_string()));
        assert!(cmd.args.contains(&"libx264".to_string()));
        assert!(cmd.args.contains(&"-c:a".to_string()));
        assert!(cmd.args.contains(&"aac".to_string()));
        assert!(cmd.args.contains(&"out.mp4".to_string()));
    }

    #[test]
    fn mpv_preview_segment_command_structure() {
        let cmd = mpv_preview_segment_command("chunk-1.mp4", 3.2, 27.0);
        assert_eq!(cmd.program, "mpv");
        assert!(cmd.args.iter().any(|a| a == "--start=3.2"));
        assert!(cmd.args.iter().any(|a| a == "--end=27"));
        assert!(cmd.args.contains(&"chunk-1.mp4".to_string()));
    }

    #[test]
    fn concat_with_positive_delay_has_adelay() {
        let cmd = ffmpeg_concat_with_audio_delay_command(&[], "output.mp4", "/tmp/list.txt", 0.10);
        assert_eq!(cmd.program, "ffmpeg");
        assert!(cmd.args.contains(&"-map".to_string()));
        assert!(cmd.args.contains(&"0:v".to_string()));
        assert!(cmd.args.contains(&"0:a".to_string()));
        assert!(cmd.args.contains(&"-c:v".to_string()));
        assert!(cmd.args.contains(&"copy".to_string()));
        assert!(cmd.args.contains(&"-c:a".to_string()));
        assert!(cmd.args.contains(&"aac".to_string()));
        assert!(cmd.args.iter().any(|a| a.starts_with("adelay=")));
    }

    #[test]
    fn concat_with_negative_delay_has_atrim() {
        let cmd = ffmpeg_concat_with_audio_delay_command(&[], "output.mp4", "/tmp/list.txt", -0.05);
        assert_eq!(cmd.program, "ffmpeg");
        assert!(cmd.args.contains(&"-c:v".to_string()));
        assert!(cmd.args.contains(&"copy".to_string()));
        assert!(cmd.args.contains(&"-c:a".to_string()));
        assert!(cmd.args.contains(&"aac".to_string()));
        assert!(cmd.args.iter().any(|a| a.contains("atrim=")));
        assert!(cmd.args.iter().any(|a| a.contains("asetpts=PTS-STARTPTS")));
    }
}
