use anyhow::Result;
use rdev::{EventType, Key};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq)]
pub enum HotkeyEvent {
    TranscribeDown,
    TranscribeUp,
    Cancel,
    /// Re-paste the last transcription. Only emitted when `repaste` is bound to a
    /// *different* key than `transcribe`. When they share a key, the daemon's
    /// tap-or-hold logic emits the repaste action directly.
    Repaste,
}

pub fn parse_key(s: &str) -> Result<Key> {
    Ok(match s.to_lowercase().as_str() {
        "ralt" | "right_alt" | "altgr" => Key::AltGr,
        "lalt" | "alt" | "left_alt" => Key::Alt,
        "rctrl" | "right_ctrl" | "right_control" => Key::ControlRight,
        "lctrl" | "ctrl" | "left_ctrl" | "left_control" | "control" => Key::ControlLeft,
        "rshift" | "right_shift" => Key::ShiftRight,
        "lshift" | "shift" | "left_shift" => Key::ShiftLeft,
        "escape" | "esc" => Key::Escape,
        "space" => Key::Space,
        "tab" => Key::Tab,
        "return" | "enter" => Key::Return,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        other => anyhow::bail!(
            "Unknown key: '{}'. Supported: RAlt, LAlt, RCtrl, LCtrl, RShift, LShift, \
             Escape, Space, Tab, Return, F1-F12",
            other
        ),
    })
}

pub fn start_hotkey_listener(
    transcribe_key_str: String,
    cancel_key_str: String,
    repaste_key_str: Option<String>,
    tx: Sender<HotkeyEvent>,
) {
    std::thread::spawn(move || {
        let transcribe_key = match parse_key(&transcribe_key_str) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("Invalid transcribe key '{}': {}", transcribe_key_str, e);
                return;
            }
        };
        let cancel_key = match parse_key(&cancel_key_str) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("Invalid cancel key '{}': {}", cancel_key_str, e);
                return;
            }
        };
        // Only wire up a dedicated repaste key if it is *different* from the transcribe key.
        // When they are the same key the daemon handles timing itself.
        let repaste_key: Option<Key> = repaste_key_str
            .filter(|r| !r.eq_ignore_ascii_case(&transcribe_key_str))
            .and_then(|r| match parse_key(&r) {
                Ok(k) => Some(k),
                Err(e) => {
                    eprintln!("Invalid repaste key: {}", e);
                    None
                }
            });

        // rdev::listen blocks in its own OS message loop
        if let Err(e) = rdev::listen(move |event: rdev::Event| {
            match event.event_type {
                EventType::KeyPress(key) => {
                    if key == transcribe_key {
                        let _ = tx.send(HotkeyEvent::TranscribeDown);
                    } else if key == cancel_key {
                        let _ = tx.send(HotkeyEvent::Cancel);
                    } else if repaste_key == Some(key) {
                        let _ = tx.send(HotkeyEvent::Repaste);
                    }
                }
                EventType::KeyRelease(key) => {
                    if key == transcribe_key {
                        let _ = tx.send(HotkeyEvent::TranscribeUp);
                    }
                }
                _ => {}
            }
        }) {
            eprintln!("Hotkey listener exited: {:?}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdev::Key;

    // ---- canonical key names ----

    #[test]
    fn parse_ralt() { assert_eq!(parse_key("RAlt").unwrap(), Key::AltGr); }

    #[test]
    fn parse_lalt() { assert_eq!(parse_key("LAlt").unwrap(), Key::Alt); }

    #[test]
    fn parse_rctrl() { assert_eq!(parse_key("RCtrl").unwrap(), Key::ControlRight); }

    #[test]
    fn parse_lctrl() { assert_eq!(parse_key("LCtrl").unwrap(), Key::ControlLeft); }

    #[test]
    fn parse_rshift() { assert_eq!(parse_key("RShift").unwrap(), Key::ShiftRight); }

    #[test]
    fn parse_lshift() { assert_eq!(parse_key("LShift").unwrap(), Key::ShiftLeft); }

    #[test]
    fn parse_escape() { assert_eq!(parse_key("Escape").unwrap(), Key::Escape); }

    #[test]
    fn parse_space() { assert_eq!(parse_key("Space").unwrap(), Key::Space); }

    #[test]
    fn parse_tab() { assert_eq!(parse_key("Tab").unwrap(), Key::Tab); }

    #[test]
    fn parse_return() { assert_eq!(parse_key("Return").unwrap(), Key::Return); }

    // ---- aliases ----

    #[test]
    fn parse_ralt_aliases() {
        assert_eq!(parse_key("right_alt").unwrap(), Key::AltGr);
        assert_eq!(parse_key("altgr").unwrap(), Key::AltGr);
    }

    #[test]
    fn parse_lalt_aliases() {
        assert_eq!(parse_key("alt").unwrap(), Key::Alt);
        assert_eq!(parse_key("left_alt").unwrap(), Key::Alt);
    }

    #[test]
    fn parse_ctrl_aliases() {
        assert_eq!(parse_key("right_ctrl").unwrap(), Key::ControlRight);
        assert_eq!(parse_key("right_control").unwrap(), Key::ControlRight);
        assert_eq!(parse_key("ctrl").unwrap(), Key::ControlLeft);
        assert_eq!(parse_key("control").unwrap(), Key::ControlLeft);
        assert_eq!(parse_key("left_control").unwrap(), Key::ControlLeft);
    }

    #[test]
    fn parse_shift_aliases() {
        assert_eq!(parse_key("right_shift").unwrap(), Key::ShiftRight);
        assert_eq!(parse_key("shift").unwrap(), Key::ShiftLeft);
        assert_eq!(parse_key("left_shift").unwrap(), Key::ShiftLeft);
    }

    #[test]
    fn parse_enter_alias() {
        assert_eq!(parse_key("Enter").unwrap(), Key::Return);
    }

    #[test]
    fn parse_esc_alias() {
        assert_eq!(parse_key("esc").unwrap(), Key::Escape);
    }

    // ---- F-keys ----

    #[test]
    fn parse_all_f_keys() {
        let expected = [
            Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6,
            Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12,
        ];
        for (i, expected_key) in expected.iter().enumerate() {
            let name = format!("F{}", i + 1);
            assert_eq!(
                parse_key(&name).unwrap(),
                *expected_key,
                "{} should parse correctly",
                name
            );
        }
    }

    // ---- case insensitivity ----

    #[test]
    fn parse_key_case_insensitive() {
        assert_eq!(parse_key("ralt").unwrap(), Key::AltGr);
        assert_eq!(parse_key("RALT").unwrap(), Key::AltGr);
        assert_eq!(parse_key("Ralt").unwrap(), Key::AltGr);
        assert_eq!(parse_key("ESCAPE").unwrap(), Key::Escape);
        assert_eq!(parse_key("f9").unwrap(), Key::F9);
    }

    // ---- unknown key ----

    #[test]
    fn parse_key_unknown_errors() {
        assert!(parse_key("UnknownKey").is_err());
        assert!(parse_key("A").is_err());
        assert!(parse_key("").is_err());
        assert!(parse_key("Ctrl+Alt+Del").is_err());
    }

    #[test]
    fn parse_key_error_message_mentions_key() {
        let err = parse_key("BadKey").unwrap_err();
        // parse_key lowercases the input before matching, so the error contains "badkey".
        assert!(
            err.to_string().contains("badkey") || err.to_string().contains("Unknown key"),
            "error should identify the unrecognised key, got: {}",
            err
        );
    }
}
