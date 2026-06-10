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
