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

    std::fs::create_dir_all(&entry_dir).context("Failed to create history entry directory")?;

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
    entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HistoryConfig;
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    fn cfg(dir: &TempDir) -> HistoryConfig {
        HistoryConfig {
            path: dir.path().to_str().unwrap().to_string(),
            ..HistoryConfig::default()
        }
    }

    // ---- history_dir ----

    #[test]
    fn history_dir_uses_custom_path() {
        let c = HistoryConfig {
            path: "/custom/history".to_string(),
            ..HistoryConfig::default()
        };
        assert_eq!(history_dir(&c), std::path::PathBuf::from("/custom/history"));
    }

    #[test]
    fn history_dir_default_contains_quoteme_and_history() {
        let c = HistoryConfig::default(); // path = ""
        let dir = history_dir(&c);
        let s = dir.to_str().unwrap();
        assert!(
            s.contains("quoteme"),
            "default history dir should be under quoteme/"
        );
        assert!(
            s.contains("history"),
            "default history dir should end in /history"
        );
    }

    // ---- save_entry / list_entries ----

    #[test]
    fn save_entry_creates_all_three_files() {
        let tmp = TempDir::new().unwrap();
        save_entry(&cfg(&tmp), "hello", &[0.1_f32; 160], 0.01, false).unwrap();

        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1, "one entry directory expected");
        let entry_dir = &entries[0];
        assert!(
            entry_dir.join("transcription.txt").exists(),
            "transcription.txt missing"
        );
        assert!(entry_dir.join("audio.wav").exists(), "audio.wav missing");
        assert!(
            entry_dir.join("metadata.json").exists(),
            "metadata.json missing"
        );
    }

    #[test]
    fn save_entry_metadata_fields_round_trip() {
        let tmp = TempDir::new().unwrap();
        save_entry(&cfg(&tmp), "test text", &[], 1.5, false).unwrap();
        let entries = list_entries(&cfg(&tmp)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "test text");
        assert!((entries[0].duration_secs - 1.5).abs() < 1e-6);
        assert!(!entries[0].cancelled);
        assert!(!entries[0].id.is_empty());
    }

    #[test]
    fn save_entry_cancelled_flag_stored() {
        let tmp = TempDir::new().unwrap();
        save_entry(&cfg(&tmp), "", &[], 0.0, true).unwrap();
        let entries = list_entries(&cfg(&tmp)).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].cancelled);
    }

    #[test]
    fn list_entries_nonexistent_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let c = HistoryConfig {
            path: tmp.path().join("nonexistent").to_str().unwrap().to_string(),
            ..HistoryConfig::default()
        };
        assert!(list_entries(&c).unwrap().is_empty());
    }

    #[test]
    fn list_entries_sorted_newest_first() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(&tmp);
        save_entry(&c, "first", &[], 1.0, false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        save_entry(&c, "second", &[], 1.0, false).unwrap();

        let entries = list_entries(&c).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "second", "newest entry must come first");
        assert_eq!(entries[1].text, "first");
    }

    #[test]
    fn list_entries_ignores_dirs_without_metadata() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(&tmp);
        // Stray directory with no metadata.json
        std::fs::create_dir(tmp.path().join("not-an-entry")).unwrap();
        save_entry(&c, "real entry", &[], 1.0, false).unwrap();

        let entries = list_entries(&c).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "real entry");
    }

    // ---- cleanup ----

    #[test]
    fn cleanup_noop_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let c = HistoryConfig {
            path: tmp.path().join("nonexistent").to_str().unwrap().to_string(),
            max_recordings: 1,
            ..HistoryConfig::default()
        };
        cleanup(&c).unwrap(); // must not error
    }

    #[test]
    fn cleanup_zero_limits_deletes_nothing() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(&tmp); // max_recordings=0, max_age_days=0
        for i in 0..5 {
            save_entry(&c, &format!("entry {}", i), &[], 1.0, false).unwrap();
        }
        cleanup(&c).unwrap();
        assert_eq!(list_entries(&c).unwrap().len(), 5);
    }

    #[test]
    fn cleanup_enforces_max_recordings() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(&tmp);
        for i in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            save_entry(&c, &format!("entry {}", i), &[], 1.0, false).unwrap();
        }
        let c_limited = HistoryConfig {
            max_recordings: 3,
            ..c
        };
        cleanup(&c_limited).unwrap();

        let remaining = list_entries(&c_limited).unwrap();
        assert_eq!(remaining.len(), 3, "should keep only 3 newest entries");
        // Newest-first order: entry 4, entry 3, entry 2
        assert_eq!(remaining[0].text, "entry 4");
        assert_eq!(remaining[1].text, "entry 3");
        assert_eq!(remaining[2].text, "entry 2");
    }

    #[test]
    fn cleanup_max_age_days_removes_old_entries() {
        let tmp = TempDir::new().unwrap();
        let c = cfg(&tmp);

        // Write an old entry directly with a timestamp 10 days in the past.
        let old_id = uuid::Uuid::new_v4().to_string();
        let entry_dir = tmp.path().join(&old_id);
        std::fs::create_dir_all(&entry_dir).unwrap();
        let old_entry = HistoryEntry {
            id: old_id.clone(),
            timestamp: Utc::now() - Duration::days(10),
            text: "old entry".to_string(),
            duration_secs: 1.0,
            cancelled: false,
        };
        std::fs::write(
            entry_dir.join("metadata.json"),
            serde_json::to_string_pretty(&old_entry).unwrap(),
        )
        .unwrap();
        std::fs::write(entry_dir.join("transcription.txt"), "old entry").unwrap();

        // A fresh entry saved normally.
        save_entry(&c, "new entry", &[], 1.0, false).unwrap();
        assert_eq!(list_entries(&c).unwrap().len(), 2);

        let c_limited = HistoryConfig {
            max_age_days: 7,
            ..c
        };
        cleanup(&c_limited).unwrap();

        let remaining = list_entries(&c_limited).unwrap();
        assert_eq!(remaining.len(), 1, "10-day-old entry should be deleted");
        assert_eq!(remaining[0].text, "new entry");
    }
}
