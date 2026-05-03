use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub file: String,
    pub duration_secs: f64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrecConfig {
    pub chunks: Vec<Chunk>,
    pub selected_screen: Option<String>,
    pub selected_microphone: Option<String>,
    pub next_chunk_number: u64,
}

impl Default for QrecConfig {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            selected_screen: None,
            selected_microphone: None,
            next_chunk_number: 1,
        }
    }
}

impl QrecConfig {
    pub fn generate_chunk_id(&self) -> String {
        format!("chunk-{}", self.next_chunk_number)
    }

    pub fn generate_chunk_filename(&self) -> String {
        format!("chunk-{}.mp4", self.next_chunk_number)
    }
}

pub fn load_or_default(json_str: &str) -> anyhow::Result<QrecConfig> {
    if json_str.trim().is_empty() {
        return Ok(QrecConfig::default());
    }
    let config: QrecConfig = serde_json::from_str(json_str)?;
    Ok(config)
}

pub fn serialize_config(config: &QrecConfig) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(config)?)
}
