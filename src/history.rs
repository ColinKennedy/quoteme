use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::HistoryConfig;

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub duration_secs: f64,
    pub cancelled: bool,
}

pub fn history_dir(config: &HistoryConfig) -> PathBuf {
    if !config.path.is_empty() {
        return PathBuf::from(&config.path);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quoteme")
        .join("history")
}

pub fn save_entry(
    config: &HistoryConfig,
    text: &str,
    audio: &[f32],
    duration_secs: f64,
    cancelled: bool,
) -> Result<()> {
    let dir = history_dir(config);
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now();
    let entry_dir = dir.join(&id);

    std::fs::create_dir_all(&entry_dir)
        .context("Failed to create history entry directory")?;

    std::fs::write(entry_dir.join("transcription.txt"), text)
        .context("Failed to write transcription")?;

    crate::audio::save_wav(
        &entry_dir.join("audio.wav"),
        audio,
        crate::audio::WHISPER_SAMPLE_RATE,
    )
    .context("Failed to save audio")?;

    let entry = HistoryEntry {
        id,
        timestamp,
        text: text.to_string(),
        duration_secs,
        cancelled,
    };
    std::fs::write(
        entry_dir.join("metadata.json"),
        serde_json::to_string_pretty(&entry)?,
    )
    .context("Failed to write metadata")?;

    Ok(())
}

pub fn list_entries(config: &HistoryConfig) -> Result<Vec<HistoryEntry>> {
    let dir = history_dir(config);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir).context("Failed to read history directory")? {
        let path = entry?.path().join("metadata.json");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            if let Ok(e) = serde_json::from_str::<HistoryEntry>(&raw) {
                entries.push(e);
            }
        }
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

pub fn cleanup(config: &HistoryConfig) -> Result<()> {
    let dir = history_dir(config);
    if !dir.exists() {
        return Ok(());
    }

    let entries = list_entries(config)?;
    let now = Utc::now();
    let mut to_delete = std::collections::HashSet::new();

    if config.max_age_days > 0 {
        let max_age = chrono::Duration::days(config.max_age_days as i64);
        for e in &entries {
            if now.signed_duration_since(e.timestamp) > max_age {
                to_delete.insert(e.id.clone());
            }
        }
    }

    if config.max_recordings > 0 && entries.len() > config.max_recordings {
        let keep: std::collections::HashSet<_> = entries
            .iter()
            .take(config.max_recordings)
            .map(|e| &e.id)
            .collect();
        for e in &entries {
            if !keep.contains(&e.id) {
                to_delete.insert(e.id.clone());
            }
        }
    }

    for id in &to_delete {
        let path = dir.join(id);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("Failed to delete history entry {}", id))?;
            tracing::info!("Deleted old history entry {}", id);
        }
    }

    Ok(())
}
