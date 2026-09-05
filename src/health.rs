use std::path::Path;

use crate::config::{HotkeysConfig, RecordingMode};

#[derive(Debug, PartialEq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug)]
pub struct CheckItem {
    pub status: Status,
    pub message: String,
}

impl CheckItem {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            message: msg.into(),
        }
    }
    pub fn warn(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            message: msg.into(),
        }
    }
    pub fn fail(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Fail,
            message: msg.into(),
        }
    }
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            status: Status::Info,
            message: msg.into(),
        }
    }
    pub fn is_ok(&self) -> bool {
        self.status == Status::Ok
    }
}

pub fn check_repaste_hotkey(hotkeys: &HotkeysConfig) -> CheckItem {
    if hotkeys.repaste.is_empty() {
        return CheckItem::info("Repaste hotkey: not configured (optional)");
    }

    if hotkeys.repaste.eq_ignore_ascii_case(&hotkeys.cancel) {
        return CheckItem::fail(format!(
            "Repaste hotkey conflict: repaste and cancel are both bound to \"{}\"",
            hotkeys.repaste
        ));
    }

    if hotkeys.repaste.eq_ignore_ascii_case(&hotkeys.transcribe)
        && hotkeys.mode == RecordingMode::PushToTalk
    {
        return CheckItem::fail(format!(
            "Repaste hotkey conflict: repaste is bound to \"{}\" (same as transcribe) \
             but mode is push_to_talk — hold is already used for recording. \
             Use a different repaste key or switch to toggle mode.",
            hotkeys.repaste
        ));
    }

    if hotkeys.repaste.eq_ignore_ascii_case(&hotkeys.transcribe) {
        CheckItem::ok(format!(
            "Repaste hotkey: \"{}\" — tap to record, hold to repaste (shared with transcribe)",
            hotkeys.repaste
        ))
    } else {
        CheckItem::ok(format!("Repaste hotkey: \"{}\"", hotkeys.repaste))
    }
}

pub fn check_hotkeys(transcribe: &str, cancel: &str) -> CheckItem {
    if transcribe == cancel {
        CheckItem::fail(format!(
            "Hotkey conflict: transcribe and cancel are both bound to \"{}\"",
            transcribe
        ))
    } else {
        CheckItem::ok(format!(
            "Hotkeys: \"{}\" (transcribe), \"{}\" (cancel) — no conflict",
            transcribe, cancel
        ))
    }
}

pub fn check_model_path(model_path: &str) -> CheckItem {
    if model_path.is_empty() {
        return CheckItem::fail(
            "Model path: not set — daemon cannot transcribe (set transcription.model_path)",
        );
    }
    let mp = Path::new(model_path);
    if !mp.exists() {
        return CheckItem::fail(format!("Model path: file not found at \"{}\"", model_path));
    }
    let ext = mp
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "bin" && ext != "gguf" {
        return CheckItem::fail(format!(
            "Model path: \"{}\" has extension .{} — whisper.cpp requires a GGML/GGUF \
             .bin file. Download one from https://huggingface.co/ggerganov/whisper.cpp",
            model_path, ext
        ));
    }
    if mp.metadata().map(|m| m.len()).unwrap_or(0) < 1_000_000 {
        return CheckItem::warn(format!(
            "Model path: \"{}\" exists but is very small — may not be a valid model",
            model_path
        ));
    }
    CheckItem::ok(format!("Model path: \"{}\"", model_path))
}

pub fn check_model_size(model_path: &str) -> CheckItem {
    let size = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
    const GB: u64 = 1_000_000_000;
    if size >= 2 * GB {
        CheckItem::warn(format!(
            "Model size: {:.1} GB — very large; transcription will be slow without a GPU \
             (consider a smaller model or the quoteme-cuda binary)",
            size as f64 / GB as f64,
        ))
    } else if size >= GB {
        CheckItem::warn(format!(
            "Model size: {:.1} GB — large; transcription may be slow on CPU \
             (consider a smaller model or the quoteme-cuda binary)",
            size as f64 / GB as f64,
        ))
    } else {
        CheckItem::ok(format!("Model size: {:.0} MB", size as f64 / 1_000_000.0))
    }
}

pub fn check_word_list(word_list_path: &str) -> CheckItem {
    if word_list_path.is_empty() {
        return CheckItem::ok("Word list: not configured (optional)");
    }
    let wlp = Path::new(word_list_path);
    if !wlp.exists() {
        CheckItem::warn(format!(
            "Word list: file not found at \"{}\"",
            word_list_path
        ))
    } else {
        CheckItem::ok(format!("Word list: \"{}\"", word_list_path))
    }
}

pub fn check_history_dir(dir: &Path) -> CheckItem {
    if dir.exists() {
        CheckItem::ok(format!("History directory: \"{}\" (exists)", dir.display()))
    } else {
        let parent_ok = dir.parent().map(|p| p.exists()).unwrap_or(false);
        if parent_ok || dir.parent().is_none() {
            CheckItem::ok(format!(
                "History directory: \"{}\" (will be created on first recording)",
                dir.display()
            ))
        } else {
            CheckItem::warn(format!(
                "History directory: \"{}\" — parent path does not exist",
                dir.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HotkeysConfig, RecordingMode};
    use std::io::Write;
    use tempfile::Builder;

    // --- hotkeys ---

    // --- repaste hotkey ---

    #[test]
    fn repaste_empty_is_info() {
        let hotkeys = HotkeysConfig::default(); // repaste = ""
        let r = check_repaste_hotkey(&hotkeys);
        assert!(matches!(r.status, Status::Info));
        assert!(r.message.contains("optional"));
    }

    #[test]
    fn repaste_same_as_cancel_is_fail() {
        let hotkeys = HotkeysConfig {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            repaste: "Escape".to_string(),
            mode: RecordingMode::Toggle,
        };
        let r = check_repaste_hotkey(&hotkeys);
        assert!(matches!(r.status, Status::Fail));
        assert!(r.message.contains("conflict"));
    }

    #[test]
    fn repaste_same_as_cancel_conflict_case_insensitive() {
        let hotkeys = HotkeysConfig {
            transcribe: "RAlt".to_string(),
            cancel: "escape".to_string(),
            repaste: "ESCAPE".to_string(),
            mode: RecordingMode::Toggle,
        };
        let r = check_repaste_hotkey(&hotkeys);
        assert!(matches!(r.status, Status::Fail));
    }

    #[test]
    fn repaste_same_as_transcribe_push_to_talk_is_fail() {
        let hotkeys = HotkeysConfig {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            repaste: "RAlt".to_string(),
            mode: RecordingMode::PushToTalk,
        };
        let r = check_repaste_hotkey(&hotkeys);
        assert!(matches!(r.status, Status::Fail));
        assert!(r.message.contains("push_to_talk"));
    }

    #[test]
    fn repaste_same_as_transcribe_toggle_is_ok() {
        let hotkeys = HotkeysConfig {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            repaste: "RAlt".to_string(),
            mode: RecordingMode::Toggle,
        };
        let r = check_repaste_hotkey(&hotkeys);
        assert!(r.is_ok(), "same key in toggle mode should be OK (tap/hold)");
        assert!(r.message.contains("tap to record"));
    }

    #[test]
    fn repaste_different_key_is_ok() {
        let hotkeys = HotkeysConfig {
            transcribe: "RAlt".to_string(),
            cancel: "Escape".to_string(),
            repaste: "F9".to_string(),
            mode: RecordingMode::Toggle,
        };
        let r = check_repaste_hotkey(&hotkeys);
        assert!(r.is_ok());
        assert!(r.message.contains("F9"));
    }

    // --- hotkeys ---

    #[test]
    fn hotkeys_conflict_is_fail() {
        let r = check_hotkeys("RAlt", "RAlt");
        assert!(matches!(r.status, Status::Fail));
        assert!(r.message.contains("conflict"));
    }

    #[test]
    fn hotkeys_no_conflict_is_ok() {
        let r = check_hotkeys("RAlt", "Escape");
        assert!(r.is_ok());
        assert!(r.message.contains("no conflict"));
    }

    #[test]
    fn hotkeys_conflict_message_includes_key_name() {
        let r = check_hotkeys("F9", "F9");
        assert!(r.message.contains("F9"));
    }

    // --- model path ---

    #[test]
    fn model_path_empty_is_fail() {
        let r = check_model_path("");
        assert!(matches!(r.status, Status::Fail));
        assert!(r.message.contains("not set"));
    }

    #[test]
    fn model_path_nonexistent_is_fail() {
        let r = check_model_path("/definitely/does/not/exist/model.bin");
        assert!(matches!(r.status, Status::Fail));
        assert!(r.message.contains("not found"));
    }

    #[test]
    fn model_path_wrong_extension_is_fail() {
        let tmp = Builder::new().suffix(".pt").tempfile().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_path(&path);
        assert!(
            matches!(r.status, Status::Fail),
            "expected fail, got {:?}: {}",
            r.status,
            r.message
        );
        assert!(r.message.contains("extension"));
    }

    #[test]
    fn model_path_safetensors_extension_is_fail() {
        let tmp = Builder::new().suffix(".safetensors").tempfile().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_path(&path);
        assert!(matches!(r.status, Status::Fail));
    }

    #[test]
    fn model_path_small_bin_is_warn() {
        let mut tmp = Builder::new().suffix(".bin").tempfile().unwrap();
        tmp.write_all(b"tiny").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_path(&path);
        assert!(
            matches!(r.status, Status::Warn),
            "expected warn, got {:?}: {}",
            r.status,
            r.message
        );
        assert!(r.message.contains("very small"));
    }

    #[test]
    fn model_path_valid_large_bin_is_ok() {
        let mut tmp = Builder::new().suffix(".bin").tempfile().unwrap();
        tmp.write_all(&vec![0u8; 1_100_000]).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_path(&path);
        assert!(r.is_ok(), "expected ok, got {:?}: {}", r.status, r.message);
    }

    // --- model size ---

    #[test]
    fn model_size_sub_gigabyte_is_ok() {
        let mut tmp = Builder::new().suffix(".bin").tempfile().unwrap();
        tmp.write_all(&vec![0u8; 1024]).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_size(&path);
        assert!(r.is_ok(), "expected ok, got {:?}: {}", r.status, r.message);
        assert!(r.message.contains("MB"));
    }

    #[test]
    fn model_size_missing_file_is_ok() {
        // Nonexistent file → metadata fails → size treated as 0 → ok "0 MB"
        let r = check_model_size("/nonexistent/model.bin");
        assert!(
            r.is_ok(),
            "missing file should not produce a warning, got {:?}",
            r.status
        );
    }

    #[test]
    fn model_path_gguf_extension_accepted() {
        let mut tmp = Builder::new().suffix(".gguf").tempfile().unwrap();
        tmp.write_all(&vec![0u8; 1_100_000]).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_model_path(&path);
        assert!(r.is_ok(), "expected ok, got {:?}: {}", r.status, r.message);
    }

    // --- word list ---

    #[test]
    fn word_list_empty_is_ok() {
        let r = check_word_list("");
        assert!(r.is_ok());
        assert!(r.message.contains("optional"));
    }

    #[test]
    fn word_list_nonexistent_is_warn() {
        let r = check_word_list("/no/such/words.txt");
        assert!(matches!(r.status, Status::Warn));
        assert!(r.message.contains("not found"));
    }

    #[test]
    fn word_list_existing_file_is_ok() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let r = check_word_list(&path);
        assert!(r.is_ok());
    }

    // --- history directory ---

    #[test]
    fn history_dir_exists_is_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let r = check_history_dir(dir.path());
        assert!(r.is_ok());
        assert!(r.message.contains("exists"));
    }

    #[test]
    fn history_dir_creatable_parent_exists_is_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let subdir = dir.path().join("history");
        let r = check_history_dir(&subdir);
        assert!(r.is_ok(), "expected ok, got {:?}: {}", r.status, r.message);
        assert!(r.message.contains("will be created"));
    }

    #[test]
    fn history_dir_unreachable_parent_is_warn() {
        let dir = tempfile::TempDir::new().unwrap();
        // parent "ghost" does not exist inside dir, so "ghost/history" has no reachable parent
        let deep = dir.path().join("ghost").join("history");
        let r = check_history_dir(&deep);
        assert!(
            matches!(r.status, Status::Warn),
            "expected warn, got {:?}: {}",
            r.status,
            r.message
        );
    }
}
