//! Title text rendering with GDI into a BGRA frame. Owner: worker `render-titles`.

use clipcheck_timeline::model::{Rgba, TextSpec};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_WORDBREAK, HDC,
    HGDIOBJ, TRANSPARENT,
};

use crate::error::RenderError;
use crate::frame::Frame;
use crate::pipeline::TitleProvider;

/// Largest accepted canvas side in pixels.
const MAX_SIDE: u32 = 8192;

/// Renders titles with GDI (`CreateFontW`, `DrawTextW` into a 32-bit DIB). Text is centred in the
/// canvas (the project positions titles with the clip transform, applied by the compositor).
/// The size is `spec.size * h / 1080`. Colour, outline (drawn by offsetting 8 directions), box
/// background and alignment follow [`TextSpec`]. Alpha is 0 where nothing is drawn.
#[derive(Debug, Default)]
pub struct GdiTitleProvider;

impl TitleProvider for GdiTitleProvider {
    fn render(&mut self, text: &TextSpec, w: u32, h: u32) -> Result<Frame, RenderError> {
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(RenderError::Invalid(format!(
                "title canvas {w}x{h} is outside 1..={MAX_SIDE}"
            )));
        }
        if text.text.trim().is_empty() {
            return Ok(Frame::solid(w, h, 0, 0, 0, 0));
        }
        render_gdi(text, w, h)
    }
}

/// Horizontal alignment of the text block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    /// Parses "left", "right" or anything else (centre).
    fn parse(s: &str) -> Align {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Align::Left,
            "right" => Align::Right,
            _ => Align::Center,
        }
    }

    /// The matching `DrawTextW` alignment flag.
    fn flag(self) -> DRAW_TEXT_FORMAT {
        match self {
            Align::Left => DT_LEFT,
            Align::Center => DT_CENTER,
            Align::Right => DT_RIGHT,
        }
    }
}

/// Accumulated straight-alpha colour (channels 0..1) used while compositing layers.
#[derive(Debug, Clone, Copy, Default)]
struct Acc {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

/// Standard "over" operator for straight alpha: `src` (colour `c`, alpha `src_a` in 0..1) on top
/// of `dst`.
fn over(dst: Acc, c: Rgba, src_a: f32) -> Acc {
    if src_a <= 0.0 {
        return dst;
    }
    let out_a = src_a + dst.a * (1.0 - src_a);
    if out_a <= 0.0 {
        return Acc::default();
    }
    let keep = dst.a * (1.0 - src_a);
    let mix = |s: u8, d: f32| (f32::from(s) / 255.0 * src_a + d * keep) / out_a;
    Acc {
        r: mix(c.r, dst.r),
        g: mix(c.g, dst.g),
        b: mix(c.b, dst.b),
        a: out_a,
    }
}

/// Deletes a device context on drop.
struct DcGuard(HDC);

impl Drop for DcGuard {
    fn drop(&mut self) {
        // SAFETY: the DC came from CreateCompatibleDC; the selection guards declared after this
        // one drop first and have already restored the stock objects.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// Deletes a GDI object (bitmap or font) on drop. The object must not be selected anywhere.
struct ObjGuard(HGDIOBJ);

impl Drop for ObjGuard {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateDIBSection or CreateFontW and is deselected
        // before this guard drops (the selection guard declared after it drops first).
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

/// Selects an object into a DC and puts the previous object back on drop.
struct Selection {
    dc: HDC,
    old: HGDIOBJ,
}

impl Selection {
    /// Selects `obj` into `dc`; None when GDI reports failure.
    fn new(dc: HDC, obj: HGDIOBJ) -> Option<Selection> {
        // SAFETY: dc and obj are live GDI handles owned by the caller.
        let old = unsafe { SelectObject(dc, obj) };
        if old.0.is_null() {
            None
        } else {
            Some(Selection { dc, old })
        }
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        // SAFETY: dc is still alive (declared before this guard) and old was returned by
        // SelectObject for it.
        unsafe {
            let _ = SelectObject(self.dc, self.old);
        }
    }
}

/// Error for a failed GDI call.
fn gdi_err(step: &'static str) -> RenderError {
    RenderError::Invalid(format!("title text: {step} failed"))
}

/// Font height in pixels: `size * h / 1080`, rounded, kept in 8..=h (8 when h is below 8).
fn font_px(size: f64, h: i32) -> i32 {
    let px = (size * f64::from(h) / 1080.0).round();
    let px = if px.is_finite() { px as i32 } else { 8 };
    px.min(h).max(8)
}

/// Draws the text as white on a black DIB at each offset and returns the coverage (max of B, G,
/// R) per pixel. `bits` is the DIB pixel buffer of `n` pixels selected into `dc`.
fn draw_mask(
    dc: HDC,
    bits: *mut u8,
    n: usize,
    wide: &mut [u16],
    base: RECT,
    flags: DRAW_TEXT_FORMAT,
    offsets: &[(i32, i32)],
) -> Vec<u8> {
    // SAFETY: bits points to the n * 4 bytes of the DIB section that is selected into dc and
    // alive for this whole call. Nothing else touches it meanwhile.
    unsafe {
        std::slice::from_raw_parts_mut(bits, n * 4).fill(0);
    }
    for &(dx, dy) in offsets {
        let mut r = RECT {
            left: base.left + dx,
            top: base.top + dy,
            right: base.right + dx,
            bottom: base.bottom + dy,
        };
        // SAFETY: dc is a live DC with the font selected; r is a valid RECT on the stack.
        unsafe {
            DrawTextW(dc, wide, &mut r, flags);
        }
    }
    // SAFETY: the GDI calls above have completed, so the DIB memory holds the drawn pixels.
    let px = unsafe { std::slice::from_raw_parts(bits as *const u8, n * 4) };
    px.as_chunks::<4>()
        .0
        .iter()
        .map(|p| p.iter().take(3).copied().max().unwrap_or(0))
        .collect()
}

/// Width and height of the text laid out wrapped inside a box `wrap_w` pixels wide. The width
/// is the widest line.
fn measure(dc: HDC, wide: &mut [u16], wrap_w: i32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: wrap_w,
        bottom: 0,
    };
    // SAFETY: dc is a live DC with the font selected; r is a valid RECT on the stack.
    unsafe {
        DrawTextW(
            dc,
            wide,
            &mut r,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_LEFT,
        );
    }
    (
        (r.right - r.left).clamp(0, wrap_w),
        (r.bottom - r.top).max(0),
    )
}

/// Renders a non-empty title with GDI. Canvas sizes are already validated.
fn render_gdi(spec: &TextSpec, w: u32, h: u32) -> Result<Frame, RenderError> {
    let (wi, hi) = (w as i32, h as i32);
    let n = (w as usize) * (h as usize);
    let font_size = font_px(spec.size, hi);
    let align = Align::parse(&spec.align);
    let mut wide: Vec<u16> = spec
        .text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .collect();

    // Device context compatible with the screen.
    // SAFETY: a null HDC asks for a memory DC compatible with the current screen.
    let dc = unsafe { CreateCompatibleDC(HDC(std::ptr::null_mut())) };
    if dc.0.is_null() {
        return Err(gdi_err("CreateCompatibleDC"));
    }
    let _dc = DcGuard(dc);

    // Top-down 32-bit DIB section of w x h.
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: wi,
            biHeight: -hi,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi is fully initialised; bits receives the pixel pointer on success.
    let bmp = unsafe { CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }
        .map_err(|_| gdi_err("CreateDIBSection"))?;
    let _bmp = ObjGuard(HGDIOBJ(bmp.0));
    if bits.is_null() {
        return Err(gdi_err("CreateDIBSection (no pixel memory)"));
    }
    let bits = bits as *mut u8;
    let _sel_bmp = Selection::new(dc, HGDIOBJ(bmp.0)).ok_or_else(|| gdi_err("SelectObject"))?;

    // Font: height is the negative character height in pixels (CreateFontW convention).
    let face_name = if spec.font.trim().is_empty() {
        "Segoe UI".to_string()
    } else {
        spec.font.clone()
    };
    let face: Vec<u16> = face_name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: face is a NUL-terminated UTF-16 string that outlives the call.
    let font = unsafe {
        CreateFontW(
            -font_size,
            0,
            0,
            0,
            if spec.bold { 700 } else { 400 },
            u32::from(spec.italic),
            0,
            0,
            1, // DEFAULT_CHARSET
            0, // OUT_DEFAULT_PRECIS
            0, // CLIP_DEFAULT_PRECIS
            4, // ANTIALIASED_QUALITY: grayscale coverage, no colour fringes
            0, // DEFAULT_PITCH | FF_DONTCARE
            PCWSTR(face.as_ptr()),
        )
    };
    if font.0.is_null() {
        return Err(gdi_err("CreateFontW"));
    }
    let _font = ObjGuard(HGDIOBJ(font.0));
    let _sel_font = Selection::new(dc, HGDIOBJ(font.0)).ok_or_else(|| gdi_err("SelectObject"))?;
    // SAFETY: dc is a live DC; these only change its state.
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
    }

    // Layout: text box is 90 % of the canvas width, centred; the block is centred vertically.
    let wrap_w = ((f64::from(wi)) * 0.9).round().max(1.0) as i32;
    let left = (wi - wrap_w) / 2;
    let (tw, th) = measure(dc, &mut wide, wrap_w);
    let top = (hi - th) / 2;
    let base = RECT {
        left,
        top,
        right: left + wrap_w,
        bottom: top + th,
    };
    let flags = DT_WORDBREAK | DT_NOPREFIX | align.flag();

    // Coverage masks: fill once, outline as the text shifted by +-ow in the 8 directions plus
    // the centre (so the outline also sits under the anti-aliased fill edge).
    let fill_cov = draw_mask(dc, bits, n, &mut wide, base, flags, &[(0, 0)]);
    let outline_cov = match spec.outline {
        Some(_) => {
            let ow = ((f64::from(font_size)) * 0.05).round().max(1.0) as i32;
            let steps = [-ow, 0, ow];
            let offsets: Vec<(i32, i32)> = steps
                .iter()
                .flat_map(|&dx| steps.iter().map(move |&dy| (dx, dy)))
                .collect();
            draw_mask(dc, bits, n, &mut wide, base, flags, &offsets)
        }
        None => vec![0u8; n],
    };

    // Background box: the widest line's extent plus padding, placed by the alignment.
    let box_rect = spec.box_bg.map(|_| {
        let pad = (f64::from(font_size) * 0.3).round() as i32;
        let x0 = match align {
            Align::Left => left,
            Align::Right => left + wrap_w - tw,
            Align::Center => left + (wrap_w - tw) / 2,
        };
        (x0 - pad, top - pad, x0 + tw + pad, top + th + pad)
    });

    // Composite: box, then outline, then fill, all straight alpha.
    let mut out = vec![0u8; n * 4];
    let cw = w as usize;
    for (i, ((px_out, &co), &cf)) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(outline_cov.iter())
        .zip(fill_cov.iter())
        .enumerate()
    {
        let x = (i % cw) as i32;
        let y = (i / cw) as i32;
        let mut acc = Acc::default();
        if let (Some(bg), Some((x0, y0, x1, y1))) = (spec.box_bg, box_rect) {
            if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                acc = over(acc, bg, f32::from(bg.a) / 255.0);
            }
        }
        if let Some(oc) = spec.outline {
            let a = f32::from(co) / 255.0 * f32::from(oc.a) / 255.0;
            acc = over(acc, oc, a);
        }
        let a = f32::from(cf) / 255.0 * f32::from(spec.color.a) / 255.0;
        acc = over(acc, spec.color, a);

        let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        let [b, g, r, a] = px_out;
        *b = to_u8(acc.b);
        *g = to_u8(acc.g);
        *r = to_u8(acc.r);
        *a = to_u8(acc.a);
    }

    Ok(Frame {
        width: w,
        height: h,
        bgra: out,
    })
}
