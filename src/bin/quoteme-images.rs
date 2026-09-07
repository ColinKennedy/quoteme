#[cfg(not(windows))]
fn main() {
    eprintln!("quoteme-images is currently supported only on Windows");
}

#[cfg(windows)]
mod windows_editor {
    use anyhow::{Context, Result};
    use image::RgbaImage;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use windows::{
        core::w,
        Win32::{
            Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
                InvalidateRect, SelectObject, SetBkMode, SetTextColor, StretchDIBits, TextOutW,
                UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, PAINTSTRUCT,
                PS_SOLID, SRCCOPY, TRANSPARENT,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Input::KeyboardAndMouse::{
                    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_RETURN,
                },
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
                    LoadCursorW, PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage,
                    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, IDC_ARROW, MSG, SW_SHOW,
                    WINDOW_EX_STYLE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
                    WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
                },
            },
        },
    };

    const HEADER_HEIGHT: i32 = 78;

    #[derive(Clone, Copy, Debug)]
    struct CropRect {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    }

    struct ImageItem {
        path: PathBuf,
        crop: Option<CropRect>,
    }

    struct Group {
        label: String,
        images: Vec<ImageItem>,
    }

    struct AppState {
        groups: Vec<Group>,
        group: usize,
        image: usize,
        zoom: f32,
        drag_start: Option<POINT>,
        selection: Option<(POINT, POINT)>,
        cache: Option<(PathBuf, RgbaImage)>,
        status: String,
    }

    static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

    pub fn run() -> Result<()> {
        unsafe {
            use windows::Win32::UI::HiDpi::{
                SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            };
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let history = history_argument();
        let groups = load_groups(&history)?;
        let status = if groups.is_empty() {
            format!("No screenshots found under {}", history.display())
        } else {
            "Drag to select a crop, then Enter to queue it and advance.".to_string()
        };
        STATE
            .set(Mutex::new(AppState {
                groups,
                group: 0,
                image: 0,
                zoom: 1.0,
                drag_start: None,
                selection: None,
                cache: None,
                status,
            }))
            .map_err(|_| anyhow::anyhow!("Editor state was already initialized"))?;

        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("QuoteMeImageEditor");
            let wc = WNDCLASSW {
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hInstance: instance.into(),
                lpszClassName: class,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                anyhow::bail!("Failed to register image editor window class");
            }
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!("QuoteMe Screenshot Review"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1200,
                800,
                None,
                HMENU(0),
                instance,
                None,
            );
            if hwnd.0 == 0 {
                anyhow::bail!("Failed to create image editor window");
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd).ok()?;
            let mut message = MSG::default();
            while GetMessageW(&mut message, HWND(0), 0, 0).into() {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn history_argument() -> PathBuf {
        let args: Vec<_> = std::env::args_os().collect();
        args.windows(2)
            .find(|pair| pair[0] == "--history")
            .map(|pair| PathBuf::from(&pair[1]))
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("quoteme")
                    .join("history")
            })
    }

    fn load_groups(history: &Path) -> Result<Vec<Group>> {
        if !history.exists() {
            return Ok(Vec::new());
        }
        let mut groups = Vec::new();
        for directory in std::fs::read_dir(history).context("Could not read history folder")? {
            let directory = directory?.path();
            if !directory.is_dir() {
                continue;
            }
            let mut paths: Vec<_> = std::fs::read_dir(&directory)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.starts_with("screenshot-") && name.ends_with(".png"))
                        .unwrap_or(false)
                })
                .collect();
            paths.sort();
            if paths.is_empty() {
                continue;
            }
            let metadata = std::fs::read_to_string(directory.join("metadata.json")).ok();
            let timestamp = metadata
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|value| value["timestamp"].as_str().map(str::to_string))
                .map(|raw| {
                    chrono::DateTime::parse_from_rfc3339(&raw)
                        .map(|timestamp| {
                            timestamp
                                .with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string()
                        })
                        .unwrap_or(raw)
                });
            let label = timestamp.unwrap_or_else(|| {
                directory
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            groups.push(Group {
                label,
                images: paths
                    .into_iter()
                    .map(|path| ImageItem { path, crop: None })
                    .collect(),
            });
        }
        groups.sort_by(|a, b| b.label.cmp(&a.label));
        Ok(groups)
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let point = point_from_lparam(lparam);
                if let Some(state) = STATE.get() {
                    if let Ok(mut state) = state.lock() {
                        state.drag_start = Some(point);
                        state.selection = Some((point, point));
                    }
                }
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(state) = STATE.get() {
                    if let Ok(mut state) = state.lock() {
                        if let Some(start) = state.drag_start {
                            state.selection = Some((start, point_from_lparam(lparam)));
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(state) = STATE.get() {
                    if let Ok(mut state) = state.lock() {
                        if let Some(start) = state.drag_start.take() {
                            state.selection = Some((start, point_from_lparam(lparam)));
                        }
                    }
                }
                let _ = ReleaseCapture();
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                handle_key(hwnd, wparam.0 as u32);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn handle_key(hwnd: HWND, key: u32) {
        let Some(global) = STATE.get() else { return };
        let Ok(mut state) = global.lock() else { return };
        match key {
            0x48 => navigate_image(&mut state, -1),
            0x4c => navigate_image(&mut state, 1),
            0x4a => navigate_group(&mut state, 1),
            0x4b => navigate_group(&mut state, -1),
            0x6b | 0xbb => state.zoom = (state.zoom * 1.2).min(8.0),
            0x6d | 0xbd => state.zoom = (state.zoom / 1.2).max(0.1),
            value if value == VK_RETURN.0 as u32 => accept_crop_and_advance(hwnd, &mut state),
            value if value == VK_ESCAPE.0 as u32 => {
                if state.selection.take().is_none() {
                    PostQuitMessage(0);
                }
            }
            0x53 if GetKeyState(VK_CONTROL.0 as i32) < 0 => save_current_batch(&mut state),
            _ => {}
        }
        let _ = InvalidateRect(hwnd, None, false);
    }

    fn navigate_image(state: &mut AppState, delta: i32) {
        let Some(group) = state.groups.get(state.group) else {
            return;
        };
        if group.images.is_empty() {
            return;
        }
        state.image = wrap(state.image, delta, group.images.len());
        reset_view(state);
    }

    fn navigate_group(state: &mut AppState, delta: i32) {
        if state.groups.is_empty() {
            return;
        }
        state.group = wrap(state.group, delta, state.groups.len());
        state.image = 0;
        reset_view(state);
    }

    fn wrap(index: usize, delta: i32, count: usize) -> usize {
        (index as i32 + delta).rem_euclid(count as i32) as usize
    }

    fn reset_view(state: &mut AppState) {
        state.zoom = 1.0;
        state.selection = None;
        state.drag_start = None;
        state.cache = None;
    }

    unsafe fn accept_crop_and_advance(hwnd: HWND, state: &mut AppState) {
        let Some(selection) = state.selection else {
            return;
        };
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let zoom = state.zoom;
        let Some(image) = current_rgba(state) else {
            return;
        };
        let display = image_rect(client, image.width(), image.height(), zoom);
        let crop = selection_to_crop(selection, display, image.width(), image.height());
        if let Some(crop) = crop {
            if let Some(item) = state
                .groups
                .get_mut(state.group)
                .and_then(|group| group.images.get_mut(state.image))
            {
                item.crop = Some(crop);
                state.status = format!("Crop queued: {}×{}", crop.width, crop.height);
            }
            navigate_image(state, 1);
        }
    }

    fn save_current_batch(state: &mut AppState) {
        let Some(group) = state.groups.get_mut(state.group) else {
            return;
        };
        let mut saved = 0;
        for item in &mut group.images {
            let Some(crop) = item.crop else { continue };
            let result = (|| -> Result<()> {
                let source = image::open(&item.path)?;
                let cropped = source.crop_imm(crop.x, crop.y, crop.width, crop.height);
                let temp = item.path.with_extension("quoteme-crop.tmp.png");
                cropped.save_with_format(&temp, image::ImageFormat::Png)?;
                let backup = item.path.with_extension("quoteme-original.bak.png");
                if backup.exists() {
                    std::fs::remove_file(&backup)?;
                }
                std::fs::rename(&item.path, &backup)?;
                if let Err(error) = std::fs::rename(&temp, &item.path) {
                    let _ = std::fs::rename(&backup, &item.path);
                    return Err(error.into());
                }
                let _ = std::fs::remove_file(backup);
                Ok(())
            })();
            match result {
                Ok(()) => {
                    item.crop = None;
                    saved += 1;
                }
                Err(error) => {
                    state.status = format!("Save failed for {}: {error}", item.path.display());
                    state.cache = None;
                    return;
                }
            }
        }
        state.cache = None;
        state.selection = None;
        state.status = format!("Saved {saved} cropped image(s) in this transcript.");
    }

    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let background = CreateSolidBrush(COLORREF(0x00202020));
        FillRect(dc, &client, background);
        let _ = DeleteObject(background);

        if let Some(global) = STATE.get() {
            if let Ok(mut state) = global.lock() {
                draw_text(dc, 14, 10, &header_text(&state), COLORREF(0x00ffffff));
                draw_text(dc, 14, 34, &state.status, COLORREF(0x0000d7ff));
                draw_text(
                    dc,
                    14,
                    55,
                    "H/L images  J/K transcripts  +/- zoom  drag crop  Enter queue+next  Ctrl+S save batch  Esc clear/exit",
                    COLORREF(0x00c0c0c0),
                );
                let zoom = state.zoom;
                if let Some(image) = current_rgba(&mut state) {
                    let display = image_rect(client, image.width(), image.height(), zoom);
                    let mut bgra = image.as_raw().clone();
                    for pixel in bgra.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                    let info = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: image.width() as i32,
                            biHeight: -(image.height() as i32),
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    StretchDIBits(
                        dc,
                        display.left,
                        display.top,
                        display.right - display.left,
                        display.bottom - display.top,
                        0,
                        0,
                        image.width() as i32,
                        image.height() as i32,
                        Some(bgra.as_ptr().cast()),
                        &info,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                    if let Some((a, b)) = state.selection {
                        draw_selection(dc, a, b);
                    }
                }
            }
        }
        let _ = EndPaint(hwnd, &ps);
    }

    fn current_rgba(state: &mut AppState) -> Option<&RgbaImage> {
        let path = state
            .groups
            .get(state.group)?
            .images
            .get(state.image)?
            .path
            .clone();
        let stale = state
            .cache
            .as_ref()
            .map(|(p, _)| p != &path)
            .unwrap_or(true);
        if stale {
            match image::open(&path) {
                Ok(image) => state.cache = Some((path, image.to_rgba8())),
                Err(error) => {
                    state.status = format!("Could not load image: {error}");
                    state.cache = None;
                }
            }
        }
        state.cache.as_ref().map(|(_, image)| image)
    }

    fn image_rect(client: RECT, width: u32, height: u32, zoom: f32) -> RECT {
        let available_w = (client.right - client.left).max(1) as f32;
        let available_h = (client.bottom - HEADER_HEIGHT).max(1) as f32;
        let fit = (available_w / width as f32).min(available_h / height as f32);
        let scale = fit * zoom;
        let w = (width as f32 * scale).round() as i32;
        let h = (height as f32 * scale).round() as i32;
        let left = (client.right - w) / 2;
        let top = HEADER_HEIGHT + ((client.bottom - HEADER_HEIGHT - h) / 2);
        RECT {
            left,
            top,
            right: left + w,
            bottom: top + h,
        }
    }

    fn selection_to_crop(
        (a, b): (POINT, POINT),
        display: RECT,
        width: u32,
        height: u32,
    ) -> Option<CropRect> {
        let left = a.x.min(b.x).clamp(display.left, display.right);
        let right = a.x.max(b.x).clamp(display.left, display.right);
        let top = a.y.min(b.y).clamp(display.top, display.bottom);
        let bottom = a.y.max(b.y).clamp(display.top, display.bottom);
        if right - left < 3 || bottom - top < 3 {
            return None;
        }
        let sx = width as f32 / (display.right - display.left) as f32;
        let sy = height as f32 / (display.bottom - display.top) as f32;
        let x = ((left - display.left) as f32 * sx).round() as u32;
        let y = ((top - display.top) as f32 * sy).round() as u32;
        let crop_width = (((right - left) as f32 * sx).round() as u32).min(width - x);
        let crop_height = (((bottom - top) as f32 * sy).round() as u32).min(height - y);
        (crop_width > 0 && crop_height > 0).then_some(CropRect {
            x,
            y,
            width: crop_width,
            height: crop_height,
        })
    }

    fn header_text(state: &AppState) -> String {
        let Some(group) = state.groups.get(state.group) else {
            return "QuoteMe screenshots".to_string();
        };
        let queued = group
            .images
            .iter()
            .filter(|image| image.crop.is_some())
            .count();
        format!(
            "Transcript {}/{}: {}    Image {}/{}    Zoom {:.0}%    {} crop(s) queued",
            state.group + 1,
            state.groups.len(),
            group.label,
            state.image + 1,
            group.images.len(),
            state.zoom * 100.0,
            queued,
        )
    }

    unsafe fn draw_text(
        dc: windows::Win32::Graphics::Gdi::HDC,
        x: i32,
        y: i32,
        text: &str,
        color: COLORREF,
    ) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, color);
        let _ = TextOutW(dc, x, y, &wide);
    }

    unsafe fn draw_selection(dc: windows::Win32::Graphics::Gdi::HDC, a: POINT, b: POINT) {
        use windows::Win32::Graphics::Gdi::{LineTo, MoveToEx};
        let pen = CreatePen(PS_SOLID, 2, COLORREF(0x0000ffff));
        let old = SelectObject(dc, pen);
        let (left, right) = (a.x.min(b.x), a.x.max(b.x));
        let (top, bottom) = (a.y.min(b.y), a.y.max(b.y));
        let _ = MoveToEx(dc, left, top, None);
        let _ = LineTo(dc, right, top);
        let _ = LineTo(dc, right, bottom);
        let _ = LineTo(dc, left, bottom);
        let _ = LineTo(dc, left, top);
        let _ = SelectObject(dc, old);
        let _ = DeleteObject(pen);
    }

    fn point_from_lparam(lparam: LPARAM) -> POINT {
        POINT {
            x: (lparam.0 as u16 as i16) as i32,
            y: ((lparam.0 >> 16) as u16 as i16) as i32,
        }
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_editor::run()
}
