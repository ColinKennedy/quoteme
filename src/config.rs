use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const ENV_CONFIG_VAR: &str = "QUOTEME_CONFIGURATION_FILE";
const APP_NAME: &str = "quoteme";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub hotkeys: HotkeysConfig,
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default)]
    pub transcription: TranscriptionConfig,
    #[serde(default)]
    pub paste: PasteConfig,
    #[serde(default)]
    pub history: HistoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeysConfig {
    pub transcribe: String,
    pub cancel: String,
    pub mode: RecordingMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Toggle,
    PushToTalk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    pub device: String,
    pub mute_system_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    pub model_path: String,
    pub language: String,
    pub word_list_path: String,
    pub unload_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteConfig {
    pub method: PasteMethod,
    pub restore_clipboard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    Immediate,
    Clipboard,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    pub path: String,
    pub max_recordings: usize,
    pub max_age_days: u64,
    pub save_cancelled: bool,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            mode: RecordingMode::Toggle,
        }
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            mute_system_audio: false,
        }
    }
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            language: "en".to_string(),
            word_list_path: String::new(),
            unload_after_secs: 300,
        }
    }
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            method: PasteMethod::Immediate,
            restore_clipboard: true,
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            max_recordings: 0,
            max_age_days: 0,
            save_cancelled: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkeys: HotkeysConfig::default(),
            recording: RecordingConfig::default(),
            transcription: TranscriptionConfig::default(),
            paste: PasteConfig::default(),
            history: HistoryConfig::default(),
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var(ENV_CONFIG_VAR) {
        return PathBuf::from(path);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join("config.toml")
}

pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    toml::from_str(&contents).context("Failed to parse config TOML")
}

pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)?;
    std::fs::write(&path, contents)?;
    Ok(())
}

pub fn set_config_value(key: &str, value: &str) -> Result<()> {
    let mut config = load_config()?;
    match key {
        "hotkeys.transcribe" => config.hotkeys.transcribe = value.to_string(),
        "hotkeys.cancel" => config.hotkeys.cancel = value.to_string(),
        "hotkeys.mode" => {
            config.hotkeys.mode = match value {
                "toggle" => RecordingMode::Toggle,
                "push_to_talk" => RecordingMode::PushToTalk,
                _ => anyhow::bail!("Invalid mode: '{}'. Use 'toggle' or 'push_to_talk'", value),
            };
        }
        "recording.device" => config.recording.device = value.to_string(),
        "recording.mute_system_audio" => {
            config.recording.mute_system_audio =
                value.parse().context("Expected 'true' or 'false'")?;
        }
        "transcription.model_path" => config.transcription.model_path = value.to_string(),
        "transcription.language" => config.transcription.language = value.to_string(),
        "transcription.word_list_path" => {
            config.transcription.word_list_path = value.to_string()
        }
        "transcription.unload_after_secs" => {
            config.transcription.unload_after_secs =
                value.parse().context("Expected a number")?;
        }
        "paste.method" => {
            config.paste.method = match value {
                "immediate" => PasteMethod::Immediate,
                "clipboard" => PasteMethod::Clipboard,
                "none" => PasteMethod::None,
                _ => anyhow::bail!(
                    "Invalid paste method: '{}'. Use 'immediate', 'clipboard', or 'none'",
                    value
                ),
            };
        }
        "paste.restore_clipboard" => {
            config.paste.restore_clipboard =
                value.parse().context("Expected 'true' or 'false'")?;
        }
        "history.path" => config.history.path = value.to_string(),
        "history.max_recordings" => {
            config.history.max_recordings = value.parse().context("Expected a number")?;
        }
        "history.max_age_days" => {
            config.history.max_age_days = value.parse().context("Expected a number")?;
        }
        "history.save_cancelled" => {
            config.history.save_cancelled =
                value.parse().context("Expected 'true' or 'false'")?;
        }
        _ => anyhow::bail!(
            "Unknown config key: '{}'. Valid keys: hotkeys.transcribe, hotkeys.cancel, hotkeys.mode, \
            recording.device, recording.mute_system_audio, transcription.model_path, \
            transcription.language, transcription.word_list_path, transcription.unload_after_secs, \
            paste.method, paste.restore_clipboard, history.path, history.max_recordings, \
            history.max_age_days, history.save_cancelled",
            key
        ),
    }
    save_config(&config)
}
