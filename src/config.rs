use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const ENV_CONFIG_VAR: &str = "QUOTEME_CONFIGURATION_FILE";
const APP_NAME: &str = "quoteme";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

fn default_hotkey_transcribe() -> String {
    "RAlt".to_string()
}
fn default_hotkey_cancel() -> String {
    "Escape".to_string()
}
fn default_recording_mode() -> RecordingMode {
    RecordingMode::Toggle
}
fn default_language() -> String {
    "en".to_string()
}
fn default_unload_after_secs() -> u64 {
    300
}
fn default_silence_timeout_secs() -> u64 {
    20
}
fn default_paste_method() -> PasteMethod {
    PasteMethod::CtrlV
}
fn default_restore_clipboard() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeysConfig {
    #[serde(default = "default_hotkey_transcribe")]
    pub transcribe: String,
    #[serde(default = "default_hotkey_cancel")]
    pub cancel: String,
    #[serde(default = "default_recording_mode")]
    pub mode: RecordingMode,
    /// Key to re-paste the last transcription. Empty = disabled.
    /// If set to the same key as `transcribe` (toggle mode only): tap = record, hold = repaste.
    #[serde(default)]
    pub repaste: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Toggle,
    PushToTalk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub mute_system_audio: bool,
    /// Seconds of silence before recording auto-stops. 0 = disabled.
    #[serde(default = "default_silence_timeout_secs")]
    pub silence_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    #[serde(default)]
    pub model_path: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub word_list_path: String,
    #[serde(default = "default_unload_after_secs")]
    pub unload_after_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteConfig {
    #[serde(default = "default_paste_method")]
    pub method: PasteMethod,
    #[serde(default = "default_restore_clipboard")]
    pub restore_clipboard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    CtrlV,
    CtrlShiftV,
    Clipboard,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryConfig {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub max_recordings: usize,
    #[serde(default)]
    pub max_age_days: u64,
    #[serde(default)]
    pub save_cancelled: bool,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            mode: RecordingMode::Toggle,
            repaste: String::new(),
        }
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            mute_system_audio: false,
            silence_timeout_secs: 20,
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
            method: PasteMethod::CtrlV,
            restore_clipboard: true,
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

pub fn reload_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join("reload")
}

pub fn load_config_from_path(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    toml::from_str(&contents).context("Failed to parse config TOML")
}

pub fn load_config() -> Result<Config> {
    load_config_from_path(&config_path())
}

fn save_config_to_path(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)?;
    std::fs::write(path, contents)?;
    Ok(())
}

fn set_config_value_at(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut config = load_config_from_path(path)?;
    match key {
        "hotkeys.transcribe" => config.hotkeys.transcribe = value.to_string(),
        "hotkeys.cancel" => config.hotkeys.cancel = value.to_string(),
        "hotkeys.repaste" => config.hotkeys.repaste = value.to_string(),
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
        "recording.silence_timeout_secs" => {
            let v: u64 = value.parse().context("Expected a positive number")?;
            if v == 0 {
                anyhow::bail!(
                    "silence_timeout_secs must be at least 1 (set a large number to effectively disable auto-stop)"
                );
            }
            config.recording.silence_timeout_secs = v;
        }
        "transcription.model_path" => config.transcription.model_path = value.to_string(),
        "transcription.language" => config.transcription.language = value.to_string(),
        "transcription.word_list_path" => config.transcription.word_list_path = value.to_string(),
        "transcription.unload_after_secs" => {
            config.transcription.unload_after_secs = value.parse().context("Expected a number")?;
        }
        "paste.method" => {
            config.paste.method = match value {
                "ctrl_v" => PasteMethod::CtrlV,
                "ctrl_shift_v" => PasteMethod::CtrlShiftV,
                "clipboard" => PasteMethod::Clipboard,
                "none" => PasteMethod::None,
                _ => anyhow::bail!(
                    "Invalid paste method: '{}'. Use 'ctrl_v', 'ctrl_shift_v', 'clipboard', or 'none'",
                    value
                ),
            };
        }
        "paste.restore_clipboard" => {
            config.paste.restore_clipboard = value.parse().context("Expected 'true' or 'false'")?;
        }
        "history.path" => config.history.path = value.to_string(),
        "history.max_recordings" => {
            config.history.max_recordings = value.parse().context("Expected a number")?;
        }
        "history.max_age_days" => {
            config.history.max_age_days = value.parse().context("Expected a number")?;
        }
        "history.save_cancelled" => {
            config.history.save_cancelled = value.parse().context("Expected 'true' or 'false'")?;
        }
        _ => {
            let keys = [
                "history.max_age_days",
                "history.max_recordings",
                "history.path",
                "history.save_cancelled",
                "hotkeys.cancel",
                "hotkeys.mode",
                "hotkeys.repaste",
                "hotkeys.transcribe",
                "paste.method",
                "paste.restore_clipboard",
                "recording.device",
                "recording.mute_system_audio",
                "recording.silence_timeout_secs",
                "transcription.language",
                "transcription.model_path",
                "transcription.unload_after_secs",
                "transcription.word_list_path",
            ];
            let list = keys
                .iter()
                .map(|k| format!("  {}", k))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("Unknown config key: '{}'\n\nValid keys:\n{}", key, list)
        }
    }
    save_config_to_path(path, &config)
}

pub fn set_config_value(key: &str, value: &str) -> Result<()> {
    set_config_value_at(&config_path(), key, value)?;
    // Signal the running daemon to reload config on its next idle tick.
    let _ = std::fs::write(reload_path(), "");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_config(dir: &TempDir, toml: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        fs::write(&path, toml).unwrap();
        path
    }

    // --- default values ---

    #[test]
    fn defaults_are_correct() {
        let cfg = Config::default();
        assert_eq!(cfg.hotkeys.transcribe, "RAlt");
        assert_eq!(cfg.hotkeys.cancel, "Escape");
        assert_eq!(cfg.hotkeys.mode, RecordingMode::Toggle);
        assert!(cfg.recording.device.is_empty());
        assert!(!cfg.recording.mute_system_audio);
        assert!(cfg.transcription.model_path.is_empty());
        assert_eq!(cfg.transcription.language, "en");
        assert!(cfg.transcription.word_list_path.is_empty());
        assert_eq!(cfg.transcription.unload_after_secs, 300);
        assert_eq!(cfg.paste.method, PasteMethod::CtrlV);
        assert!(cfg.paste.restore_clipboard);
        assert!(cfg.history.path.is_empty());
        assert_eq!(cfg.history.max_recordings, 0);
        assert_eq!(cfg.history.max_age_days, 0);
        assert!(!cfg.history.save_cancelled);
    }

    // --- load_config_from_path ---

    #[test]
    fn missing_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.transcribe, "RAlt");
        assert_eq!(cfg.transcription.language, "en");
    }

    #[test]
    fn partial_toml_fills_in_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            r#"
            [transcription]
            model_path = "/some/model.bin"
        "#,
        );
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.transcription.model_path, "/some/model.bin");
        assert_eq!(cfg.transcription.language, "en");
        assert_eq!(cfg.hotkeys.transcribe, "RAlt");
    }

    #[test]
    fn full_toml_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            r#"
            [hotkeys]
            transcribe = "F9"
            cancel = "F10"
            mode = "push_to_talk"
            [recording]
            device = "Blue Yeti"
            mute_system_audio = true
            [transcription]
            model_path = "C:/models/ggml-medium.bin"
            language = "fr"
            word_list_path = "words.txt"
            unload_after_secs = 0
            [paste]
            method = "clipboard"
            restore_clipboard = false
            [history]
            path = "C:/history"
            max_recordings = 100
            max_age_days = 30
            save_cancelled = true
        "#,
        );
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.transcribe, "F9");
        assert_eq!(cfg.hotkeys.cancel, "F10");
        assert_eq!(cfg.hotkeys.mode, RecordingMode::PushToTalk);
        assert_eq!(cfg.recording.device, "Blue Yeti");
        assert!(cfg.recording.mute_system_audio);
        assert_eq!(cfg.transcription.model_path, "C:/models/ggml-medium.bin");
        assert_eq!(cfg.transcription.language, "fr");
        assert_eq!(cfg.transcription.word_list_path, "words.txt");
        assert_eq!(cfg.transcription.unload_after_secs, 0);
        assert_eq!(cfg.paste.method, PasteMethod::Clipboard);
        assert!(!cfg.paste.restore_clipboard);
        assert_eq!(cfg.history.path, "C:/history");
        assert_eq!(cfg.history.max_recordings, 100);
        assert_eq!(cfg.history.max_age_days, 30);
        assert!(cfg.history.save_cancelled);
    }

    #[test]
    fn invalid_toml_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "not valid [[[toml");
        let err = load_config_from_path(&path).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("parse"));
    }

    // --- set_config_value_at ---

    #[test]
    fn set_value_creates_file_and_updates_field() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "hotkeys.transcribe", "F9").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.transcribe, "F9");
    }

    #[test]
    fn set_value_preserves_other_fields() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            r#"
            [transcription]
            model_path = "/models/whisper.bin"
            language = "de"
        "#,
        );
        set_config_value_at(&path, "hotkeys.transcribe", "F9").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.transcribe, "F9");
        assert_eq!(cfg.transcription.model_path, "/models/whisper.bin");
        assert_eq!(cfg.transcription.language, "de");
    }

    #[test]
    fn set_value_push_to_talk_mode() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "hotkeys.mode", "push_to_talk").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.mode, RecordingMode::PushToTalk);
    }

    #[test]
    fn set_value_toggle_mode() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "hotkeys.mode", "push_to_talk").unwrap();
        set_config_value_at(&path, "hotkeys.mode", "toggle").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.mode, RecordingMode::Toggle);
    }

    #[test]
    fn set_value_paste_method_variants() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        for (input, expected) in [
            ("clipboard", PasteMethod::Clipboard),
            ("none", PasteMethod::None),
            ("ctrl_v", PasteMethod::CtrlV),
            ("ctrl_shift_v", PasteMethod::CtrlShiftV),
        ] {
            set_config_value_at(&path, "paste.method", input).unwrap();
            let cfg = load_config_from_path(&path).unwrap();
            assert_eq!(cfg.paste.method, expected);
        }
    }

    #[test]
    fn set_value_numeric_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "transcription.unload_after_secs", "60").unwrap();
        set_config_value_at(&path, "history.max_recordings", "50").unwrap();
        set_config_value_at(&path, "history.max_age_days", "7").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.transcription.unload_after_secs, 60);
        assert_eq!(cfg.history.max_recordings, 50);
        assert_eq!(cfg.history.max_age_days, 7);
    }

    #[test]
    fn set_value_bool_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "recording.mute_system_audio", "true").unwrap();
        set_config_value_at(&path, "paste.restore_clipboard", "false").unwrap();
        set_config_value_at(&path, "history.save_cancelled", "true").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert!(cfg.recording.mute_system_audio);
        assert!(!cfg.paste.restore_clipboard);
        assert!(cfg.history.save_cancelled);
    }

    // --- error cases ---

    #[test]
    fn set_value_invalid_mode_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "hotkeys.mode", "hold").unwrap_err();
        assert!(err.to_string().contains("Invalid mode"));
    }

    #[test]
    fn set_value_invalid_paste_method_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "paste.method", "foobar").unwrap_err();
        assert!(err.to_string().contains("Invalid paste method"));
        assert!(err.to_string().contains("ctrl_v"));
    }

    #[test]
    fn set_value_invalid_bool_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "recording.mute_system_audio", "yes").unwrap_err();
        assert!(err.to_string().contains("Expected 'true' or 'false'"));
    }

    #[test]
    fn set_value_invalid_number_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err =
            set_config_value_at(&path, "transcription.unload_after_secs", "fast").unwrap_err();
        assert!(err.to_string().contains("Expected a number"));
    }

    #[test]
    fn set_value_unknown_key_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "nonexistent.key", "value").unwrap_err();
        assert!(err.to_string().contains("Unknown config key"));
    }

    #[test]
    fn set_value_unknown_key_lists_valid_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "bad.key", "value").unwrap_err();
        // Error message must enumerate valid keys so user knows what to use.
        assert!(err.to_string().contains("hotkeys.transcribe"));
        assert!(err.to_string().contains("transcription.model_path"));
    }

    #[test]
    fn set_value_silence_timeout_zero_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err = set_config_value_at(&path, "recording.silence_timeout_secs", "0").unwrap_err();
        assert!(err.to_string().contains("at least 1"));
    }

    #[test]
    fn set_value_silence_timeout_valid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "recording.silence_timeout_secs", "5").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.recording.silence_timeout_secs, 5);
    }

    #[test]
    fn set_value_remaining_string_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "hotkeys.repaste", "F9").unwrap();
        set_config_value_at(&path, "recording.device", "Blue Yeti").unwrap();
        set_config_value_at(&path, "transcription.language", "fr").unwrap();
        set_config_value_at(&path, "transcription.word_list_path", "/tmp/words.txt").unwrap();
        set_config_value_at(&path, "history.path", "/tmp/hist").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.hotkeys.repaste, "F9");
        assert_eq!(cfg.recording.device, "Blue Yeti");
        assert_eq!(cfg.transcription.language, "fr");
        assert_eq!(cfg.transcription.word_list_path, "/tmp/words.txt");
        assert_eq!(cfg.history.path, "/tmp/hist");
    }

    #[test]
    fn set_value_transcription_model_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        set_config_value_at(&path, "transcription.model_path", "/models/ggml-medium.bin").unwrap();
        let cfg = load_config_from_path(&path).unwrap();
        assert_eq!(cfg.transcription.model_path, "/models/ggml-medium.bin");
    }

    #[test]
    fn set_value_silence_timeout_non_numeric_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let err =
            set_config_value_at(&path, "recording.silence_timeout_secs", "never").unwrap_err();
        assert!(err.to_string().contains("positive number") || err.to_string().contains("number"));
    }
}
