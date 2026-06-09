use anyhow::{Context, Result};
use arboard::Clipboard;

use crate::config::PasteMethod;

pub fn paste_text(text: &str, method: &PasteMethod, restore_clipboard: bool) -> Result<()> {
    match method {
        PasteMethod::None => {}
        PasteMethod::Clipboard => {
            set_clipboard(text)?;
            tracing::info!("Text copied to clipboard ({} chars)", text.len());
        }
        PasteMethod::Immediate => {
            let saved = if restore_clipboard {
                get_clipboard_text().ok()
            } else {
                None
            };

            set_clipboard(text)?;
            // Give the clipboard a moment to settle before simulating keystrokes.
            std::thread::sleep(std::time::Duration::from_millis(80));
            simulate_ctrl_v()?;
            std::thread::sleep(std::time::Duration::from_millis(120));

            if restore_clipboard {
                if let Some(prev) = saved {
                    let _ = set_clipboard(&prev);
                } else {
                    // Clear clipboard so it looks untouched
                    let _ = clear_clipboard();
                }
            }
            tracing::info!("Text pasted immediately ({} chars)", text.len());
        }
    }
    Ok(())
}

pub fn get_clipboard_text() -> Result<String> {
    Clipboard::new()
        .context("Failed to open clipboard")?
        .get_text()
        .context("Failed to get clipboard text")
}

pub fn set_clipboard(text: &str) -> Result<()> {
    Clipboard::new()
        .context("Failed to open clipboard")?
        .set_text(text)
        .context("Failed to set clipboard text")
}

fn clear_clipboard() -> Result<()> {
    Clipboard::new()
        .context("Failed to open clipboard")?
        .clear()
        .context("Failed to clear clipboard")
}

fn simulate_ctrl_v() -> Result<()> {
    use enigo::{Enigo, Key, KeyboardControllable};
    let mut enigo = Enigo::new();
    enigo.key_down(Key::Control);
    enigo.key_click(Key::Layout('v'));
    enigo.key_up(Key::Control);
    Ok(())
}
