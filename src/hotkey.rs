use anyhow::Result;
use rdev::{EventType, Key};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub enum HotkeyEvent {
    TranscribeDown,
    TranscribeUp,
    Cancel,
    /// Re-paste the last transcription. Only emitted when `repaste` is bound to a
    /// *different* key than `transcribe`. When they share a key, the daemon's
    /// tap-or-hold logic emits the repaste action directly.
    Repaste,
    OpenImageEditor,
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
        "a" => Key::KeyA,
        "b" => Key::KeyB,
        "c" => Key::KeyC,
        "d" => Key::KeyD,
        "e" => Key::KeyE,
        "f" => Key::KeyF,
        "g" => Key::KeyG,
        "h" => Key::KeyH,
        "i" => Key::KeyI,
        "j" => Key::KeyJ,
        "k" => Key::KeyK,
        "l" => Key::KeyL,
        "m" => Key::KeyM,
        "n" => Key::KeyN,
        "o" => Key::KeyO,
        "p" => Key::KeyP,
        "q" => Key::KeyQ,
        "r" => Key::KeyR,
        "s" => Key::KeyS,
        "t" => Key::KeyT,
        "u" => Key::KeyU,
        "v" => Key::KeyV,
        "w" => Key::KeyW,
        "x" => Key::KeyX,
        "y" => Key::KeyY,
        "z" => Key::KeyZ,
        other => anyhow::bail!(
            "Unknown key: '{}'. Supported: RAlt, LAlt, RCtrl, LCtrl, RShift, LShift, \
             Escape, Space, Tab, Return, A-Z, F1-F12",
            other
        ),
    })
}

/// A parsed hotkey: zero or more modifier keys that must be held simultaneously,
/// plus a trigger key whose press fires the event.
///
/// Use `+` as separator in config strings, e.g. `"Ctrl+Space"` or `"LCtrl+Shift+F9"`.
/// All parts except the last are modifiers; the last part is the trigger.
#[derive(Debug, Clone, PartialEq)]
pub struct Hotkey {
    pub modifiers: Vec<Key>,
    pub trigger: Key,
}

/// Parse a hotkey string like `"RAlt"`, `"Ctrl+Space"`, or `"LCtrl+Shift+F9"`.
pub fn parse_hotkey(s: &str) -> Result<Hotkey> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        anyhow::bail!("Invalid hotkey '{}': empty key name in expression", s);
    }
    let (modifier_strs, trigger_slice) = parts.split_at(parts.len() - 1);
    let trigger = parse_key(trigger_slice[0])?;
    let modifiers = modifier_strs
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>>>()?;
    Ok(Hotkey { modifiers, trigger })
}

fn modifiers_held(held: &[Key], hotkey: &Hotkey) -> bool {
    // Match the complete modifier set. Without this, Ctrl+F10 also matches bare
    // F10, so a repaste shortcut can unexpectedly toggle recording.
    let held_modifiers: Vec<&Key> = held.iter().filter(|key| is_modifier(key)).collect();
    if held_modifiers.len() != hotkey.modifiers.len() {
        return false;
    }
    let mut remaining = held_modifiers;
    hotkey.modifiers.iter().all(|expected| {
        let Some(index) = remaining
            .iter()
            .position(|actual| same_modifier_family(expected, actual))
        else {
            return false;
        };
        remaining.remove(index);
        true
    })
}

fn same_modifier_family(a: &Key, b: &Key) -> bool {
    a == b
        || (matches!(a, Key::ControlLeft | Key::ControlRight)
            && matches!(b, Key::ControlLeft | Key::ControlRight))
        || (matches!(a, Key::ShiftLeft | Key::ShiftRight)
            && matches!(b, Key::ShiftLeft | Key::ShiftRight))
        || (matches!(a, Key::Alt | Key::AltGr) && matches!(b, Key::Alt | Key::AltGr))
}

fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::ControlLeft
            | Key::ControlRight
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::Alt
            | Key::AltGr
    )
}

/// Mutable state shared across `grab` callback invocations. `rdev::grab` requires a `Fn`
/// (not `FnMut`) callback since it may be re-entered from the OS hook, so this is held
/// behind a `Mutex` rather than captured by value.
struct ListenerState {
    /// Keys currently held down. Used to track modifier state and suppress auto-repeat.
    held: Vec<Key>,
    /// True between TranscribeDown and TranscribeUp so we don't emit spurious TranscribeUp
    /// events when the trigger key is pressed without its required modifiers.
    transcribe_active: bool,
    /// True while the current transcribe keypress is being swallowed (consume_transcribe_key).
    trigger_consumed: bool,
}

pub fn start_hotkey_listener(
    transcribe_key_str: String,
    cancel_key_str: String,
    repaste_key_str: Option<String>,
    image_editor_key_str: Option<String>,
    consume_transcribe_key: bool,
    tx: Sender<HotkeyEvent>,
) {
    std::thread::spawn(move || {
        let transcribe = match parse_hotkey(&transcribe_key_str) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!("Invalid transcribe key '{}': {}", transcribe_key_str, e);
                return;
            }
        };
        let cancel = match parse_hotkey(&cancel_key_str) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!("Invalid cancel key '{}': {}", cancel_key_str, e);
                return;
            }
        };
        // Only wire up a dedicated repaste key if it is *different* from the transcribe key.
        // When they are the same key the daemon handles timing itself.
        let repaste: Option<Hotkey> = repaste_key_str
            .filter(|r| !r.eq_ignore_ascii_case(&transcribe_key_str))
            .and_then(|r| match parse_hotkey(&r) {
                Ok(k) => Some(k),
                Err(e) => {
                    tracing::error!("Invalid repaste key: {}", e);
                    None
                }
            });
        let image_editor = image_editor_key_str.and_then(|raw| match parse_hotkey(&raw) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::error!("Invalid image editor hotkey '{}': {}", raw, error);
                None
            }
        });

        tracing::debug!(
            "Hotkey listener starting — transcribe={:?} cancel={:?} repaste={:?} editor={:?} \
             consume_transcribe_key={}",
            transcribe,
            cancel,
            repaste,
            image_editor,
            consume_transcribe_key,
        );

        let state = Mutex::new(ListenerState {
            held: Vec::new(),
            transcribe_active: false,
            trigger_consumed: false,
        });

        let callback = move |event: rdev::Event| -> Option<rdev::Event> {
            match event.event_type {
                EventType::KeyPress(key) => {
                    let mut st = state.lock().expect("hotkey listener state mutex");
                    tracing::trace!("KeyPress({:?}) held={:?}", key, st.held);
                    // Suppress duplicate events from OS key auto-repeat.
                    let is_first_press = !st.held.contains(&key);
                    if is_first_press {
                        st.held.push(key);
                    }

                    if key == transcribe.trigger {
                        if is_first_press && modifiers_held(&st.held, &transcribe) {
                            st.transcribe_active = true;
                            st.trigger_consumed = consume_transcribe_key;
                            let _ = tx.send(HotkeyEvent::TranscribeDown);
                        }
                        if let Some(ref editor) = image_editor {
                            if key == editor.trigger && modifiers_held(&st.held, editor) {
                                let _ = tx.send(HotkeyEvent::OpenImageEditor);
                            }
                        }
                        if st.trigger_consumed {
                            // Swallow the press (and any auto-repeats) so it isn't
                            // also typed into the focused application.
                            return None;
                        }
                    }
                    if is_first_press && key == cancel.trigger && modifiers_held(&st.held, &cancel)
                    {
                        let _ = tx.send(HotkeyEvent::Cancel);
                    }
                    if is_first_press {
                        if let Some(ref rp) = repaste {
                            if key == rp.trigger && modifiers_held(&st.held, rp) {
                                let _ = tx.send(HotkeyEvent::Repaste);
                            }
                        }
                    }
                    Some(event)
                }
                EventType::KeyRelease(key) => {
                    let mut st = state.lock().expect("hotkey listener state mutex");
                    tracing::trace!("KeyRelease({:?}) held={:?}", key, st.held);
                    let mut consumed = false;
                    if key == transcribe.trigger && st.transcribe_active {
                        st.transcribe_active = false;
                        consumed = st.trigger_consumed;
                        st.trigger_consumed = false;
                        let _ = tx.send(HotkeyEvent::TranscribeUp);
                    }
                    st.held.retain(|k| k != &key);
                    if consumed {
                        None
                    } else {
                        Some(event)
                    }
                }
                _ => Some(event),
            }
        };

        if let Err(e) = rdev::grab(callback) {
            tracing::error!("Hotkey listener exited with error: {:?}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdev::Key;

    // ---- parse_key: canonical names ----

    #[test]
    fn parse_ralt() {
        assert_eq!(parse_key("RAlt").unwrap(), Key::AltGr);
    }

    #[test]
    fn parse_lalt() {
        assert_eq!(parse_key("LAlt").unwrap(), Key::Alt);
    }

    #[test]
    fn parse_rctrl() {
        assert_eq!(parse_key("RCtrl").unwrap(), Key::ControlRight);
    }

    #[test]
    fn parse_lctrl() {
        assert_eq!(parse_key("LCtrl").unwrap(), Key::ControlLeft);
    }

    #[test]
    fn parse_rshift() {
        assert_eq!(parse_key("RShift").unwrap(), Key::ShiftRight);
    }

    #[test]
    fn parse_lshift() {
        assert_eq!(parse_key("LShift").unwrap(), Key::ShiftLeft);
    }

    #[test]
    fn parse_escape() {
        assert_eq!(parse_key("Escape").unwrap(), Key::Escape);
    }

    #[test]
    fn parse_space() {
        assert_eq!(parse_key("Space").unwrap(), Key::Space);
    }

    #[test]
    fn parse_tab() {
        assert_eq!(parse_key("Tab").unwrap(), Key::Tab);
    }

    #[test]
    fn parse_return() {
        assert_eq!(parse_key("Return").unwrap(), Key::Return);
    }

    // ---- parse_key: aliases ----

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

    // ---- parse_key: F-keys ----

    #[test]
    fn parse_all_f_keys() {
        let expected = [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
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

    // ---- parse_key: case insensitivity ----

    #[test]
    fn parse_key_case_insensitive() {
        assert_eq!(parse_key("ralt").unwrap(), Key::AltGr);
        assert_eq!(parse_key("RALT").unwrap(), Key::AltGr);
        assert_eq!(parse_key("Ralt").unwrap(), Key::AltGr);
        assert_eq!(parse_key("ESCAPE").unwrap(), Key::Escape);
        assert_eq!(parse_key("f9").unwrap(), Key::F9);
    }

    // ---- parse_key: unknown ----

    #[test]
    fn parse_key_unknown_errors() {
        assert!(parse_key("UnknownKey").is_err());
        assert!(parse_key("").is_err());
    }

    #[test]
    fn parse_key_error_message_mentions_key() {
        let err = parse_key("BadKey").unwrap_err();
        assert!(
            err.to_string().contains("badkey") || err.to_string().contains("Unknown key"),
            "error should identify the unrecognised key, got: {}",
            err
        );
    }

    // ---- parse_hotkey: single key (no modifiers) ----

    #[test]
    fn parse_hotkey_single_key() {
        let h = parse_hotkey("RAlt").unwrap();
        assert_eq!(h.trigger, Key::AltGr);
        assert!(h.modifiers.is_empty());
    }

    #[test]
    fn parse_hotkey_single_space() {
        let h = parse_hotkey("Space").unwrap();
        assert_eq!(h.trigger, Key::Space);
        assert!(h.modifiers.is_empty());
    }

    #[test]
    fn parse_hotkey_single_f9() {
        let h = parse_hotkey("F9").unwrap();
        assert_eq!(h.trigger, Key::F9);
        assert!(h.modifiers.is_empty());
    }

    // ---- parse_hotkey: combos ----

    #[test]
    fn parse_hotkey_ctrl_space() {
        let h = parse_hotkey("Ctrl+Space").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlLeft]);
        assert_eq!(h.trigger, Key::Space);
    }

    #[test]
    fn parse_hotkey_lctrl_space() {
        let h = parse_hotkey("LCtrl+Space").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlLeft]);
        assert_eq!(h.trigger, Key::Space);
    }

    #[test]
    fn parse_hotkey_rctrl_space() {
        let h = parse_hotkey("RCtrl+Space").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlRight]);
        assert_eq!(h.trigger, Key::Space);
    }

    #[test]
    fn parse_hotkey_two_modifiers() {
        let h = parse_hotkey("LCtrl+Shift+F9").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlLeft, Key::ShiftLeft]);
        assert_eq!(h.trigger, Key::F9);
    }

    #[test]
    fn parse_hotkey_alt_f4() {
        let h = parse_hotkey("LAlt+F4").unwrap();
        assert_eq!(h.modifiers, vec![Key::Alt]);
        assert_eq!(h.trigger, Key::F4);
    }

    #[test]
    fn parse_hotkey_case_insensitive() {
        let h = parse_hotkey("ctrl+space").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlLeft]);
        assert_eq!(h.trigger, Key::Space);
    }

    #[test]
    fn parse_hotkey_trims_whitespace() {
        let h = parse_hotkey("Ctrl + Space").unwrap();
        assert_eq!(h.modifiers, vec![Key::ControlLeft]);
        assert_eq!(h.trigger, Key::Space);
    }

    // ---- parse_hotkey: errors ----

    #[test]
    fn parse_hotkey_unknown_modifier_errors() {
        assert!(parse_hotkey("Win+Space").is_err());
    }

    #[test]
    fn parse_hotkey_unknown_trigger_errors() {
        assert!(parse_hotkey("Ctrl+Del").is_err());
    }

    #[test]
    fn parse_hotkey_empty_errors() {
        assert!(parse_hotkey("").is_err());
    }

    #[test]
    fn parse_hotkey_trailing_plus_errors() {
        assert!(parse_hotkey("Ctrl+").is_err());
    }

    #[test]
    fn parse_hotkey_leading_plus_errors() {
        assert!(parse_hotkey("+Space").is_err());
    }

    // ---- modifiers_held ----

    #[test]
    fn modifiers_held_no_modifiers_rejects_modified_keypress() {
        let h = parse_hotkey("Space").unwrap();
        assert!(modifiers_held(&[], &h));
        assert!(!modifiers_held(&[Key::ControlLeft], &h));
    }

    #[test]
    fn modifiers_held_with_modifier_requires_it() {
        let h = parse_hotkey("Ctrl+Space").unwrap();
        assert!(!modifiers_held(&[], &h));
        assert!(!modifiers_held(&[Key::ShiftLeft], &h));
        assert!(modifiers_held(&[Key::ControlLeft], &h));
        assert!(!modifiers_held(&[Key::ControlLeft, Key::ShiftLeft], &h));
    }

    #[test]
    fn modifiers_held_two_modifiers_requires_both() {
        let h = parse_hotkey("LCtrl+Shift+F9").unwrap();
        assert!(!modifiers_held(&[Key::ControlLeft], &h));
        assert!(!modifiers_held(&[Key::ShiftLeft], &h));
        assert!(modifiers_held(&[Key::ControlLeft, Key::ShiftLeft], &h));
    }
}
