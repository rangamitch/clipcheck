//! Always-on-top "clip saved" notification, independent of the ClipCheck window (like Medal's).
//!
//! Design (Windows):
//! - One lazily started dedicated thread, `clipcheck-toast`, owns a layered, click-through,
//!   never-activating, topmost popup window plus its message loop. [`clip_saved`] and
//!   [`message`] only push the text on an mpsc channel and post a thread message to wake the
//!   loop, so they return immediately and never panic.
//! - The pill (dark rounded rectangle, 1 px border, logo, text) is
//!   rasterised in software into a premultiplied BGRA buffer (SDF anti-aliasing). Only the
//!   text mask comes from GDI (white-on-black grayscale AA text used as coverage), which
//!   avoids GDI's alpha-less drawing. The buffer is pushed with `UpdateLayeredWindow`
//!   (`ULW_ALPHA`), which gives true per-pixel alpha and the fade via `SourceConstantAlpha`.
//! - A 16 ms thread timer drives the animation: ~150 ms fade/slide in, 2.5 s hold, ~250 ms
//!   fade out. A toast arriving while one is visible replaces the text and restarts the hold
//!   (no stacking).
//! - Placed top-centre of the primary monitor's work area, 24 px (DPI scaled) below its top.
//!   The thread is per-monitor-DPI-aware (v2) and scales all metrics by the monitor DPI.
//! - If no desktop/window is available the thread logs once and exits; later calls are
//!   silently dropped. On non-Windows targets both functions are no-ops.

/// Formats the "Saved the last ..." text for a clip of `seconds` length.
///
/// `<60` -> `45s`; `60..3600` -> `2m` (plus ` 30s` when not a whole minute and under 10
/// minutes); `>=3600` -> `1h 5m`.
pub fn format_clip_text(seconds: u32) -> String {
    let dur = if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        let m = seconds / 60;
        let s = seconds % 60;
        if s != 0 && m < 10 {
            format!("{m}m {s}s")
        } else {
            format!("{m}m")
        }
    } else {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    };
    format!("Saved the last {dur}")
}

/// Shows "Saved the last <duration>" with the app logo. Never blocks the caller.
pub fn clip_saved(seconds: u32) {
    imp::show(format_clip_text(seconds));
}

/// Shows a short message in the same overlay (errors such as a failed clip).
pub fn message(text: &str) {
    imp::show(text.to_string());
}

#[cfg(not(windows))]
mod imp {
    pub fn show(_text: String) {}
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, POINT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GetDC,
        GetMonitorInfoW, GetTextExtentPoint32W, MonitorFromPoint, ReleaseDC, SelectObject,
        SetBkMode, SetTextColor, TextOutW, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::HiDpi::{
        GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        MDT_EFFECTIVE_DPI,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW,
        PostThreadMessageW, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
        UpdateLayeredWindow, HWND_TOPMOST, MSG, PM_NOREMOVE, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_APP, WM_TIMER, WM_USER, WNDCLASSEXW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        WS_POPUP,
    };

    const LOGO_PNG: &[u8] = include_bytes!("../../ui/src-tauri/icons/128x128.png");

    const WM_WAKE: u32 = WM_APP + 1;
    const FRAME_MS: u32 = 16;
    const FADE_IN: Duration = Duration::from_millis(150);
    const HOLD: Duration = Duration::from_millis(2500);
    const FADE_OUT: Duration = Duration::from_millis(250);
    const MAX_CHARS: usize = 64;

    static SENDER: Mutex<Option<Sender<String>>> = Mutex::new(None);
    static THREAD_ID: AtomicU32 = AtomicU32::new(0);

    pub fn show(text: String) {
        let _ = std::panic::catch_unwind(|| show_inner(text));
    }

    fn show_inner(text: String) {
        let Ok(mut guard) = SENDER.lock() else { return };
        if guard.is_none() {
            let (tx, rx) = channel::<String>();
            let spawned = std::thread::Builder::new()
                .name("clipcheck-toast".into())
                .spawn(move || ui_thread(rx));
            if spawned.is_err() {
                crate::logging::warn("toast: could not start the toast thread");
                return;
            }
            *guard = Some(tx);
        }
        if let Some(tx) = guard.as_ref() {
            if tx.send(text).is_err() {
                return;
            }
        }
        drop(guard);
        let tid = THREAD_ID.load(Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: plain thread-message post; failure (thread gone) is harmless.
            unsafe {
                let _ = PostThreadMessageW(tid, WM_WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }

    // ---------------------------------------------------------------- UI thread

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn ui_thread(rx: Receiver<String>) {
        // SAFETY: all Win32 objects below are created, used and destroyed on this thread.
        unsafe {
            let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut msg = MSG::default();
            // Force creation of this thread's message queue before publishing the thread id.
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
            THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);

            let Some(hwnd) = create_window() else {
                crate::logging::warn("toast: no desktop window available; toasts disabled");
                if let Ok(mut g) = SENDER.lock() {
                    *g = None;
                }
                return;
            };
            let mut toast = Toast::new(hwnd);
            toast.drain(&rx);

            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.message == WM_WAKE && msg.hwnd.0.is_null() {
                    toast.drain(&rx);
                } else if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
                    toast.tick();
                } else {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    unsafe fn create_window() -> Option<HWND> {
        let hmod = GetModuleHandleW(None).ok()?;
        let hinst = HINSTANCE(hmod.0);
        let class = w!("ClipCheckToast");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // 0 when already registered; CreateWindowExW decides.
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            class,
            w!("ClipCheck toast"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinst,
            None,
        )
        .ok()
    }

    struct Toast {
        hwnd: HWND,
        mem_dc: HDC,
        bitmap: HBITMAP,
        text: String,
        scale_dpi: u32,
        w: i32,
        h: i32,
        t0: Instant,
        visible: bool,
        timer: usize,
        work: Option<(i32, i32, i32, i32)>,
    }

    impl Toast {
        unsafe fn new(hwnd: HWND) -> Self {
            Toast {
                hwnd,
                mem_dc: CreateCompatibleDC(None),
                bitmap: HBITMAP::default(),
                text: String::new(),
                scale_dpi: 0,
                w: 1,
                h: 1,
                t0: Instant::now(),
                visible: false,
                timer: 0,
                work: None,
            }
        }

        fn drain(&mut self, rx: &Receiver<String>) {
            let mut latest = None;
            while let Ok(t) = rx.try_recv() {
                latest = Some(t);
            }
            if let Some(t) = latest {
                // SAFETY: called on the owning thread.
                unsafe { self.start(t) };
            }
        }

        unsafe fn start(&mut self, text: String) {
            let mut text = text.replace(['\r', '\n'], " ");
            if text.chars().count() > MAX_CHARS {
                text = text.chars().take(MAX_CHARS - 1).collect::<String>() + "\u{2026}";
            }
            let (work, dpi) = primary_work_area();
            if text != self.text || dpi != self.scale_dpi || self.bitmap.is_invalid() {
                self.rebuild(&text, dpi);
                self.text = text;
                self.scale_dpi = dpi;
            }
            let now = Instant::now();
            let e = now.saturating_duration_since(self.t0);
            if !self.visible {
                self.t0 = now;
            } else if e <= FADE_IN + HOLD {
                if e > FADE_IN {
                    self.t0 = now - FADE_IN;
                }
            } else {
                // Fading out: resume fading in from the current opacity.
                let out = (e - FADE_IN - HOLD).as_secs_f32() / FADE_OUT.as_secs_f32();
                let a = (1.0 - out).clamp(0.0, 1.0);
                self.t0 = now - FADE_IN.mul_f32(a);
            }
            self.work = Some(work);
            self.present();
            if !self.visible {
                self.visible = true;
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            if self.timer == 0 {
                self.timer = SetTimer(None, 0, FRAME_MS, None);
            }
        }

        unsafe fn rebuild(&mut self, text: &str, dpi: u32) {
            let scale = dpi as f32 / 96.0;
            let mask = text_mask(self.mem_dc, text, (15.0 * scale).round() as i32);
            let frame = render_frame(&mask, scale);
            self.w = frame.w;
            self.h = frame.h;
            let (bmp, bits) = match make_dib(self.mem_dc, frame.w, frame.h) {
                Some(v) => v,
                None => return,
            };
            std::ptr::copy_nonoverlapping(frame.bgra.as_ptr(), bits, frame.bgra.len());
            let old = SelectObject(self.mem_dc, HGDIOBJ(bmp.0));
            if !self.bitmap.is_invalid() {
                // `old` is the previous toast bitmap (still selected until now).
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
            self.bitmap = bmp;
        }

        /// Draws the current animation frame (opacity and slide) for time `t0..now`.
        unsafe fn present(&mut self) {
            if self.bitmap.is_invalid() {
                return;
            }
            let Some(work) = self.work else { return };
            let scale = self.scale_dpi as f32 / 96.0;
            let e = Instant::now().saturating_duration_since(self.t0);
            let (alpha, slide) = if e < FADE_IN {
                let x = e.as_secs_f32() / FADE_IN.as_secs_f32();
                let ease = 1.0 - (1.0 - x).powi(3);
                (ease, (1.0 - ease) * -10.0 * scale)
            } else if e < FADE_IN + HOLD {
                (1.0, 0.0)
            } else {
                let x = (e - FADE_IN - HOLD).as_secs_f32() / FADE_OUT.as_secs_f32();
                ((1.0 - x).clamp(0.0, 1.0), 0.0)
            };
            let x = work.0 + ((work.2 - work.0) - self.w) / 2;
            let y = work.1 + (24.0 * scale).round() as i32 + slide.round() as i32;
            let dst = POINT { x, y };
            let size = SIZE {
                cx: self.w,
                cy: self.h,
            };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: 0, // AC_SRC_OVER
                BlendFlags: 0,
                SourceConstantAlpha: (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                AlphaFormat: 1, // AC_SRC_ALPHA
            };
            let screen = GetDC(None);
            let _ = UpdateLayeredWindow(
                self.hwnd,
                screen,
                Some(&dst),
                Some(&size),
                self.mem_dc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen);
        }

        fn tick(&mut self) {
            if !self.visible {
                return;
            }
            // SAFETY: called on the owning thread.
            unsafe {
                if self.t0.elapsed() >= FADE_IN + HOLD + FADE_OUT {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                    self.visible = false;
                    if self.timer != 0 {
                        let _ = KillTimer(None, self.timer);
                        self.timer = 0;
                    }
                } else {
                    self.present();
                }
            }
        }
    }

    impl Drop for Toast {
        fn drop(&mut self) {
            // SAFETY: owned GDI objects, on the creating thread.
            unsafe {
                let _ = DeleteDC(self.mem_dc);
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                }
            }
        }
    }

    /// Work area (left, top, right, bottom) and effective DPI of the primary monitor.
    unsafe fn primary_work_area() -> ((i32, i32, i32, i32), u32) {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut work = (0, 0, 1920, 1080);
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            work = (r.left, r.top, r.right, r.bottom);
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let dpi = match GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) {
            Ok(()) if dx >= 48 => dx,
            _ => 96,
        };
        (work, dpi)
    }

    // ---------------------------------------------------------------- GDI helpers

    /// Creates a top-down 32-bit DIB section; returns the bitmap and its pixel pointer.
    unsafe fn make_dib(dc: HDC, w: i32, h: i32) -> Option<(HBITMAP, *mut u8)> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            return None;
        }
        Some((bmp, bits as *mut u8))
    }

    struct Mask {
        w: i32,
        h: i32,
        cov: Vec<u8>,
    }

    /// Renders `text` white-on-black with grayscale AA and returns it as a coverage mask.
    unsafe fn text_mask(dc: HDC, text: &str, px: i32) -> Mask {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let font = CreateFontW(
            -px.max(8),
            0,
            0,
            0,
            600, // semibold
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            4, // ANTIALIASED_QUALITY (grayscale)
            0,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &wide, &mut sz);
        let (w, h) = (sz.cx.max(1) + 2, sz.cy.max(1) + 2);
        let mut cov = vec![0u8; (w * h) as usize];
        if let Some((bmp, bits)) = make_dib(dc, w, h) {
            let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));
            std::ptr::write_bytes(bits, 0, (w * h * 4) as usize);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            let _ = TextOutW(dc, 1, 1, &wide);
            let px_data = std::slice::from_raw_parts(bits, (w * h * 4) as usize);
            for (i, c) in cov.iter_mut().enumerate() {
                *c = px_data[i * 4 + 1];
            }
            SelectObject(dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
        }
        SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        Mask { w, h, cov }
    }

    // ---------------------------------------------------------------- software rendering

    struct Frame {
        w: i32,
        h: i32,
        /// Premultiplied BGRA, top-down.
        bgra: Vec<u8>,
    }

    type Px = [f32; 4]; // premultiplied r, g, b, a in 0..1

    fn over(dst: &mut Px, rgb: [f32; 3], a: f32) {
        if a <= 0.0 {
            return;
        }
        let k = 1.0 - a;
        dst[0] = rgb[0] * a + dst[0] * k;
        dst[1] = rgb[1] * a + dst[1] * k;
        dst[2] = rgb[2] * a + dst[2] * k;
        dst[3] = a + dst[3] * k;
    }

    fn hex(c: u32) -> [f32; 3] {
        [
            ((c >> 16) & 0xFF) as f32 / 255.0,
            ((c >> 8) & 0xFF) as f32 / 255.0,
            (c & 0xFF) as f32 / 255.0,
        ]
    }

    /// Decoded logo, RGBA8 straight alpha.
    fn logo() -> Option<&'static (u32, u32, Vec<u8>)> {
        static LOGO: OnceLock<Option<(u32, u32, Vec<u8>)>> = OnceLock::new();
        LOGO.get_or_init(|| {
            let mut dec = png::Decoder::new(LOGO_PNG);
            dec.set_transformations(png::Transformations::EXPAND);
            let mut reader = dec.read_info().ok()?;
            let mut buf = vec![0u8; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).ok()?;
            if info.bit_depth != png::BitDepth::Eight {
                return None;
            }
            let n = (info.width * info.height) as usize;
            let rgba = match info.color_type {
                png::ColorType::Rgba => buf[..n * 4].to_vec(),
                png::ColorType::Rgb => (0..n)
                    .flat_map(|i| [buf[i * 3], buf[i * 3 + 1], buf[i * 3 + 2], 255])
                    .collect(),
                _ => return None,
            };
            Some((info.width, info.height, rgba))
        })
        .as_ref()
    }

    /// Box-filter downscale of the logo to `size` x `size`, returned premultiplied.
    fn scaled_logo(size: i32) -> Option<Vec<Px>> {
        let (sw, sh, data) = logo()?;
        let (sw, sh) = (*sw as usize, *sh as usize);
        let size = size.max(1) as usize;
        let mut out = vec![[0.0f32; 4]; size * size];
        for dy in 0..size {
            let y0 = dy * sh / size;
            let y1 = (((dy + 1) * sh).div_ceil(size)).clamp(y0 + 1, sh);
            for dx in 0..size {
                let x0 = dx * sw / size;
                let x1 = (((dx + 1) * sw).div_ceil(size)).clamp(x0 + 1, sw);
                let mut acc = [0.0f32; 4];
                let mut count = 0.0;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let p = &data[(sy * sw + sx) * 4..][..4];
                        let a = p[3] as f32 / 255.0;
                        acc[0] += p[0] as f32 / 255.0 * a;
                        acc[1] += p[1] as f32 / 255.0 * a;
                        acc[2] += p[2] as f32 / 255.0 * a;
                        acc[3] += a;
                        count += 1.0;
                    }
                }
                out[dy * size + dx] = [
                    acc[0] / count,
                    acc[1] / count,
                    acc[2] / count,
                    acc[3] / count,
                ];
            }
        }
        Some(out)
    }

    fn render_frame(text: &Mask, s: f32) -> Frame {
        let r = |v: f32| (v * s).round() as i32;
        let (pad_l, logo_sz, gap1, pad_r) = (r(12.0), r(28.0), r(10.0), r(16.0));
        let h = r(46.0);
        let w = pad_l + logo_sz + gap1 + text.w + pad_r;
        let mut px: Vec<Px> = vec![[0.0; 4]; (w * h) as usize];

        // Pill: SDF rounded rectangle with a 1 px border.
        let radius = 12.0 * s;
        let bw = (1.0 * s).max(1.0);
        let (fill, border) = (hex(0x16181d), hex(0x34373f));
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        for y in 0..h {
            for x in 0..w {
                let (qx, qy) = (
                    (x as f32 + 0.5 - hw).abs() - (hw - radius),
                    (y as f32 + 0.5 - hh).abs() - (hh - radius),
                );
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0)
                    - radius;
                let outer = (0.5 - d).clamp(0.0, 1.0);
                let inner = (0.5 - (d + bw)).clamp(0.0, 1.0);
                let mut p: Px = [0.0; 4];
                over(&mut p, border, 0.94 * outer * (1.0 - inner));
                over(&mut p, fill, 0.94 * outer * inner);
                // Compose properly: border ring first then fill inside it.
                px[(y * w + x) as usize] = p;
            }
        }

        // Logo.
        let lx = pad_l;
        let ly = (h - logo_sz) / 2;
        if let Some(l) = scaled_logo(logo_sz) {
            for y in 0..logo_sz {
                for x in 0..logo_sz {
                    let c = l[(y * logo_sz + x) as usize];
                    let dst = &mut px[((ly + y) * w + lx + x) as usize];
                    let k = 1.0 - c[3];
                    for i in 0..4 {
                        dst[i] = c[i] + dst[i] * k;
                    }
                }
            }
        }

        // Text (white).
        let tx = pad_l + logo_sz + gap1;
        let ty = (h - text.h) / 2;
        for y in 0..text.h {
            for x in 0..text.w {
                let a = text.cov[(y * text.w + x) as usize] as f32 / 255.0;
                let (px_x, px_y) = (tx + x, ty + y);
                if a > 0.0 && px_x >= 0 && px_x < w && px_y >= 0 && px_y < h {
                    over(&mut px[(px_y * w + px_x) as usize], [1.0, 1.0, 1.0], a);
                }
            }
        }

        let mut bgra = Vec::with_capacity(px.len() * 4);
        for p in &px {
            let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
            bgra.extend_from_slice(&[q(p[2]), q(p[1]), q(p[0]), q(p[3])]);
        }
        Frame { w, h, bgra }
    }
}

#[cfg(test)]
mod tests {
    use super::format_clip_text;

    #[test]
    fn seconds_only() {
        assert_eq!(format_clip_text(0), "Saved the last 0s");
        assert_eq!(format_clip_text(45), "Saved the last 45s");
        assert_eq!(format_clip_text(59), "Saved the last 59s");
    }

    #[test]
    fn minutes() {
        assert_eq!(format_clip_text(60), "Saved the last 1m");
        assert_eq!(format_clip_text(120), "Saved the last 2m");
        assert_eq!(format_clip_text(90), "Saved the last 1m 30s");
        assert_eq!(format_clip_text(599), "Saved the last 9m 59s");
        assert_eq!(format_clip_text(630), "Saved the last 10m");
        assert_eq!(format_clip_text(3599), "Saved the last 59m");
    }

    #[test]
    fn hours() {
        assert_eq!(format_clip_text(3600), "Saved the last 1h");
        assert_eq!(format_clip_text(3900), "Saved the last 1h 5m");
        assert_eq!(format_clip_text(7260), "Saved the last 2h 1m");
    }
}
