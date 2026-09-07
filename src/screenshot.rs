use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    },
    UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetForegroundWindow, GetIconInfo, GetWindowRect, CURSORINFO,
        CURSOR_SHOWING, DI_NORMAL,
    },
};

/// Capture the currently active top-level window as PNG, drawing the current
/// system cursor at its exact screen position when it lies over that window.
#[cfg(windows)]
pub fn capture_active_window(path: &Path) -> Result<()> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            anyhow::bail!("Windows did not report an active window");
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).context("Failed to read active-window bounds")?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            anyhow::bail!("Active window has invalid dimensions {}x{}", width, height);
        }

        let screen_dc = GetDC(HWND(0));
        if screen_dc.0 == 0 {
            anyhow::bail!("Failed to acquire the screen device context");
        }
        let memory_dc = CreateCompatibleDC(screen_dc);
        if memory_dc.0 == 0 {
            let _ = ReleaseDC(HWND(0), screen_dc);
            anyhow::bail!("Failed to create screenshot device context");
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.0 == 0 {
            let _ = DeleteDC(memory_dc);
            let _ = ReleaseDC(HWND(0), screen_dc);
            anyhow::bail!("Failed to allocate screenshot bitmap");
        }
        let old = SelectObject(memory_dc, bitmap);

        let result = (|| -> Result<Vec<u8>> {
            BitBlt(
                memory_dc, 0, 0, width, height, screen_dc, rect.left, rect.top, SRCCOPY,
            )
            .context("Failed to copy the active window pixels")?;

            let mut cursor = CURSORINFO {
                cbSize: std::mem::size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut cursor).is_ok()
                && cursor.flags == CURSOR_SHOWING
                && cursor.ptScreenPos.x >= rect.left
                && cursor.ptScreenPos.x < rect.right
                && cursor.ptScreenPos.y >= rect.top
                && cursor.ptScreenPos.y < rect.bottom
            {
                let mut icon = Default::default();
                let (mut x, mut y) = (
                    cursor.ptScreenPos.x - rect.left,
                    cursor.ptScreenPos.y - rect.top,
                );
                if GetIconInfo(cursor.hCursor, &mut icon).is_ok() {
                    x -= icon.xHotspot as i32;
                    y -= icon.yHotspot as i32;
                    if icon.hbmMask.0 != 0 {
                        let _ = DeleteObject(icon.hbmMask);
                    }
                    if icon.hbmColor.0 != 0 {
                        let _ = DeleteObject(icon.hbmColor);
                    }
                }
                let _ = DrawIconEx(memory_dc, x, y, cursor.hCursor, 0, 0, 0, None, DI_NORMAL);
            }

            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down pixels
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0_u8; width as usize * height as usize * 4];
            let rows = GetDIBits(
                memory_dc,
                bitmap,
                0,
                height as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            if rows == 0 {
                anyhow::bail!("Failed to read screenshot bitmap pixels");
            }
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            Ok(bgra)
        })();

        let _ = SelectObject(memory_dc, old);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
        let _ = ReleaseDC(HWND(0), screen_dc);

        let rgba = result?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        image::save_buffer_with_format(
            path,
            &rgba,
            width as u32,
            height as u32,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .with_context(|| format!("Failed to save screenshot {}", path.display()))?;
        Ok(())
    }
}

#[cfg(windows)]
pub fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
pub fn enable_dpi_awareness() {}

#[cfg(not(windows))]
pub fn capture_active_window(_path: &Path) -> Result<()> {
    anyhow::bail!("Active-window screenshots are currently supported only on Windows")
}

pub fn next_path(entry_dir: &Path, number: usize) -> PathBuf {
    entry_dir.join(format!("screenshot-{number:03}.png"))
}

/// Replace command occurrences in order, preserving the command's original
/// capitalization and putting each absolute path immediately after it.
pub fn inject_paths(text: &str, phrase: &str, paths: &[PathBuf]) -> String {
    if phrase.trim().is_empty() || paths.is_empty() {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let needle = phrase.to_lowercase();
    let mut result = String::with_capacity(text.len() + paths.len() * 80);
    let mut byte_at = 0;
    let mut search_at = 0;
    let mut inserted = 0;
    for path in paths {
        let Some(relative) = lower[search_at..].find(&needle) else {
            break;
        };
        let found = search_at + relative;
        let end = found + needle.len();
        result.push_str(&text[byte_at..end]);
        result.push(' ');
        result.push('"');
        result.push_str(&path.display().to_string());
        result.push('"');
        byte_at = end;
        search_at = end;
        inserted += 1;
    }
    result.push_str(&text[byte_at..]);
    // A low-latency probe can hear the command even when the final Whisper pass
    // spells it differently. Never orphan a screenshot in that case.
    for path in &paths[inserted..] {
        if !result.trim().is_empty() {
            result.push(' ');
        }
        result.push_str(phrase);
        result.push(' ');
        result.push('"');
        result.push_str(&path.display().to_string());
        result.push('"');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_inserted_case_insensitively_and_in_order() {
        let paths = vec![PathBuf::from("C:/one.png"), PathBuf::from("C:/two.png")];
        assert_eq!(
            inject_paths("Use This Here then this here.", "this here", &paths),
            "Use This Here \"C:/one.png\" then this here \"C:/two.png\"."
        );
    }
}
