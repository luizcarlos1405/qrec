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
