//! CPU reference compositor (M4). Pure Rust, used as the v1 export compositor and as the oracle
//! for any future GPU path. Owner: worker `render-ref`.
//!
//! Performance notes: per-layer sampling taps (source index and bilinear weight per canvas column
//! and row) are computed once, pixel loops only do table lookups, opaque pixels of unit-opacity
//! layers are copied directly, and large canvases are split into disjoint row bands that run on
//! scoped threads. Every arithmetic expression matches the original per-pixel formulation, so the
//! output is byte-identical.

use clipcheck_timeline::model::{FitMode, Rgba, Transform};

use crate::frame::Frame;

/// Canvas/resize row count at which row bands are split across threads.
const PAR_MIN_ROWS: usize = 256;
/// Upper bound on worker threads for one band-parallel pass.
const MAX_THREADS: usize = 8;

/// Where a source frame lands on the canvas after fit/fill/stretch/custom, crop, scale, position.
/// All values in canvas pixels; the rectangle may extend beyond the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DestRect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// One layer to blend: a decoded source frame plus how to place it.
#[derive(Debug, Clone)]
pub struct LayerInput<'a> {
    /// Source pixels (already the right moment in time).
    pub frame: &'a Frame,
    /// Geometry.
    pub transform: &'a Transform,
    /// 0..=1.
    pub opacity: f64,
}

/// Clamps a fraction into 0..=1. Non-finite values map to 0.
fn frac(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `v` when finite, otherwise `fallback`.
fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/// Rounds and clamps a float channel value to a byte.
fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// True when the frame has a non-zero size and a buffer of exactly `width * height * 4` bytes.
fn is_valid(frame: &Frame) -> bool {
    if frame.width == 0 || frame.height == 0 {
        return false;
    }
    (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|p| p.checked_mul(4))
        == Some(frame.bgra.len())
}

/// The empty rectangle used for degenerate input.
fn zero_rect() -> DestRect {
    DestRect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    }
}

/// Computes the destination rectangle. Rules: crop removes the given fractions from the source
/// first; rotate_90 swaps width/height of the cropped source (flips apply when sampling, not here);
/// Fit = uniform scale so the whole cropped source is inside the canvas, centred; Fill = uniform
/// scale so the canvas is covered, centred; Stretch = exact canvas size; Custom = Fit size times
/// `transform.scale`, centred, then offset by `pos_x * canvas_w`, `pos_y * canvas_h`. For Fit/Fill/
/// Stretch the `scale`/`pos` fields are ignored.
pub fn dest_rect(src_w: u32, src_h: u32, canvas_w: u32, canvas_h: u32, t: &Transform) -> DestRect {
    let cw = f64::from(canvas_w);
    let ch = f64::from(canvas_h);
    let keep_w = 1.0 - frac(t.crop.left) - frac(t.crop.right);
    let keep_h = 1.0 - frac(t.crop.top) - frac(t.crop.bottom);
    let sw = f64::from(src_w) * keep_w.max(0.0);
    let sh = f64::from(src_h) * keep_h.max(0.0);
    // Oriented (rotated) size of the cropped source.
    let (ow, oh) = if t.rotate_90 % 2 == 1 {
        (sh, sw)
    } else {
        (sw, sh)
    };
    if !(ow > 0.0 && oh > 0.0 && cw > 0.0 && ch > 0.0) {
        return zero_rect();
    }
    let fit_scale = (cw / ow).min(ch / oh);
    let (w, h, off_x, off_y) = match t.fit {
        FitMode::Fit => (ow * fit_scale, oh * fit_scale, 0.0, 0.0),
        FitMode::Fill => {
            let s = (cw / ow).max(ch / oh);
            (ow * s, oh * s, 0.0, 0.0)
        }
        FitMode::Stretch => (cw, ch, 0.0, 0.0),
        FitMode::Custom => {
            if !(t.scale.is_finite() && t.scale > 0.0) {
                return zero_rect();
            }
            let s = fit_scale * t.scale;
            (
                ow * s,
                oh * s,
                finite_or(t.pos_x, 0.0) * cw,
                finite_or(t.pos_y, 0.0) * ch,
            )
        }
    };
    DestRect {
        x: (cw - w) / 2.0 + off_x,
        y: (ch - h) / 2.0 + off_y,
        w,
        h,
    }
}

/// Orientation (quarter turns and flips) and crop of a source, used to map canvas coordinates
/// back into the source. Pixel storage is passed separately (see [`Plan`]).
struct Source {
    /// Crop origin in source pixels.
    ox: usize,
    /// Crop origin in source pixels.
    oy: usize,
    /// Cropped width in pixels (>= 1).
    cw: usize,
    /// Cropped height in pixels (>= 1).
    ch: usize,
    /// Quarter turns clockwise (0..=3).
    rot: u8,
    /// Mirror left-right, applied before rotation.
    flip_h: bool,
    /// Mirror top-bottom, applied before rotation.
    flip_v: bool,
}

impl Source {
    /// Maps a point in oriented-source coordinates (after rotation) back to cropped-source
    /// coordinates (before rotation and flips), both continuous with pixel centres at +0.5.
    fn map(&self, u: f64, v: f64) -> (f64, f64) {
        let cw = self.cw as f64;
        let ch = self.ch as f64;
        let (mut fx, mut fy) = match self.rot {
            1 => (v, ch - u),
            2 => (cw - u, ch - v),
            3 => (cw - v, u),
            _ => (u, v),
        };
        if self.flip_h {
            fx = cw - fx;
        }
        if self.flip_v {
            fy = ch - fy;
        }
        (fx, fy)
    }
}

/// Widens a BGRA pixel to floats.
fn to_f(p: [u8; 4]) -> [f32; 4] {
    let [b, g, r, a] = p;
    [f32::from(b), f32::from(g), f32::from(r), f32::from(a)]
}

/// Component-wise linear interpolation.
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for ((o, x), y) in out.iter_mut().zip(a).zip(b) {
        *o = x + (y - x) * t;
    }
    out
}

/// Writes a float BGRA pixel into a 4-byte slice.
fn store(px: &mut [u8], s: [f32; 4]) {
    let [sb, sg, sr, sa] = s;
    if let [b, g, r, a] = px {
        *b = to_u8(sb);
        *g = to_u8(sg);
        *r = to_u8(sr);
        *a = to_u8(sa);
    }
}

/// Alpha-blends a source pixel (`s`, straight alpha 0..=255) over a canvas pixel in place.
/// The effective alpha is `s.alpha / 255 * opacity`.
fn blend_px(px: &mut [u8], s: [f32; 4], opacity: f32) {
    let [sb, sg, sr, sa] = s;
    let a = (sa / 255.0 * opacity).clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let ia = 1.0 - a;
    if let [b, g, r, al] = px {
        let da = f32::from(*al) / 255.0;
        *b = to_u8(sb * a + f32::from(*b) * ia);
        *g = to_u8(sg * a + f32::from(*g) * ia);
        *r = to_u8(sr * a + f32::from(*r) * ia);
        *al = to_u8((a + da * ia) * 255.0);
    }
}

/// Canvas pixel range `[start, end)` whose centres fall inside `[start, start + len)`, clipped
/// to `0..max`. None when empty.
fn span(start: f64, len: f64, max: u32) -> Option<(usize, usize)> {
    let s = (start - 0.5).ceil().max(0.0);
    let e = (start + len - 0.5).ceil().min(f64::from(max));
    if e <= s {
        None
    } else {
        Some((s as usize, e as usize))
    }
}

/// Crop origin and length along one axis of a source of `len` (>= 1) pixels. At least one
/// pixel always remains.
fn crop_span(len: u32, lo: f64, hi: f64) -> (usize, usize) {
    let total = len as usize;
    let lf = f64::from(len);
    let a = ((frac(lo) * lf) as usize).min(total - 1);
    let b = (frac(hi) * lf) as usize;
    let n = total.saturating_sub(a).saturating_sub(b).max(1);
    (a, n.min(total - a))
}

/// True when `v` is within a tiny tolerance of an integer.
fn is_integral(v: f64) -> bool {
    (v - v.round()).abs() < 1e-6
}

/// Sampling taps for one canvas column or row: the nearest source index and the two bilinear
/// neighbours with their weight. All indices are absolute (crop origin included).
#[derive(Debug, Clone, Copy)]
struct Tap {
    /// Nearest source index.
    near: usize,
    /// Lower bilinear neighbour (edge clamped).
    lo: usize,
    /// Upper bilinear neighbour (edge clamped).
    hi: usize,
    /// Weight of `hi` (0..=1).
    t: f32,
}

/// Builds the taps for a continuous cropped-source coordinate `f` along an axis of `n` (>= 1)
/// pixels whose crop starts at `origin`. Same arithmetic as nearest/bilinear sampling per pixel.
fn tap(f: f64, n: usize, origin: usize) -> Tap {
    let last = n.saturating_sub(1);
    let lastf = last as f64;
    let near = f.floor().clamp(0.0, lastf) as usize;
    let x = (f - 0.5).clamp(0.0, lastf);
    let x0 = x.floor();
    let lo = x0 as usize;
    Tap {
        near: near + origin,
        lo: lo + origin,
        hi: (lo + 1).min(last) + origin,
        t: (x - x0) as f32,
    }
}

/// Bilinear sample from two source rows given the column tap and the row weight.
fn sample_taps(top: &[u8], bot: &[u8], cx: Tap, ty: f32) -> [f32; 4] {
    let t = lerp4(to_f(bgra_at(top, cx.lo)), to_f(bgra_at(top, cx.hi)), cx.t);
    let b = lerp4(to_f(bgra_at(bot, cx.lo)), to_f(bgra_at(bot, cx.hi)), cx.t);
    lerp4(t, b, ty)
}

/// Pixel `x` of a packed BGRA row; transparent black when out of range.
fn bgra_at(row: &[u8], x: usize) -> [u8; 4] {
    match row.get(x * 4..x * 4 + 4) {
        Some(&[b, g, r, a]) => [b, g, r, a],
        _ => [0; 4],
    }
}

/// A solid frame of `width` x `height` filled with `px` (b, g, r, a).
fn solid_frame(width: u32, height: u32, px: [u8; 4]) -> Frame {
    let n = (width as usize).saturating_mul(height as usize);
    let mut bgra = vec![0u8; n.saturating_mul(4)];
    for p in bgra.as_chunks_mut::<4>().0 {
        p.copy_from_slice(&px);
    }
    Frame {
        width,
        height,
        bgra,
    }
}

/// Number of worker threads for a band-parallel pass (1 when unavailable).
fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, MAX_THREADS)
}

/// Splits `region` (whole canvas rows of `cs` bytes, one entry of `per_row` per row) into
/// disjoint row bands and runs `job` on each, in parallel for large regions.
fn run_bands<T: Sync>(
    region: &mut [u8],
    cs: usize,
    per_row: &[T],
    job: impl Fn(&mut [u8], &[T]) + Sync,
) {
    let nrows = per_row.len();
    let threads = if nrows >= PAR_MIN_ROWS && cs > 0 {
        thread_count()
    } else {
        1
    };
    if threads <= 1 {
        job(region, per_row);
        return;
    }
    let band_rows = nrows.div_ceil(threads).max(1);
    let job = &job;
    std::thread::scope(|s| {
        for (band, rows) in region
            .chunks_mut(band_rows * cs)
            .zip(per_row.chunks(band_rows))
        {
            s.spawn(move || job(band, rows));
        }
    });
}

/// Everything the per-pixel loops need about one layer.
struct Plan<'a> {
    /// Packed BGRA pixels of the whole source frame.
    src: &'a [u8],
    /// Bytes per source row.
    sstride: usize,
    /// Layer opacity (0 < opacity <= 1).
    opacity: f32,
    /// Nearest sampling is exact (1:1, unrotated, pixel-aligned).
    fast: bool,
}

/// Blends one band of canvas rows for an unrotated layer using precomputed column and row taps.
/// `band` holds whole canvas rows of `cs` bytes; `rows` has one tap per row of the band.
fn blend_axis(band: &mut [u8], cs: usize, x0: usize, cols: &[Tap], rows: &[Tap], plan: &Plan<'_>) {
    for (row, ry) in band.chunks_exact_mut(cs).zip(rows) {
        let Some(seg) = row.get_mut(x0 * 4..(x0 + cols.len()) * 4) else {
            continue;
        };
        let near_row = row_of(plan.src, plan.sstride, ry.near);
        let top = row_of(plan.src, plan.sstride, ry.lo);
        let bot = row_of(plan.src, plan.sstride, ry.hi);
        for (px, &cx) in seg.as_chunks_mut::<4>().0.iter_mut().zip(cols) {
            if plan.fast {
                let [b, g, r, a] = bgra_at(near_row, cx.near);
                if a == 0 {
                    // Zero alpha leaves the canvas unchanged.
                    continue;
                }
                if a == 255 && plan.opacity >= 1.0 {
                    // Opaque at full opacity: the blend reduces to a copy.
                    px.copy_from_slice(&[b, g, r, a]);
                } else {
                    blend_px(px, to_f([b, g, r, a]), plan.opacity);
                }
            } else {
                blend_px(px, sample_taps(top, bot, cx, ry.t), plan.opacity);
            }
        }
    }
}

/// Blends one band of canvas rows for a rotated or flipped layer. `vs` holds the oriented-source
/// row coordinate of each band row; `us` the column coordinates of the layer.
fn blend_general(
    band: &mut [u8],
    cs: usize,
    x0: usize,
    us: &[f64],
    vs: &[f64],
    src: &Source,
    plan: &Plan<'_>,
) {
    for (row, &v) in band.chunks_exact_mut(cs).zip(vs) {
        let Some(seg) = row.get_mut(x0 * 4..(x0 + us.len()) * 4) else {
            continue;
        };
        for (px, &u) in seg.as_chunks_mut::<4>().0.iter_mut().zip(us) {
            let (fx, fy) = src.map(u, v);
            let cx = tap(fx, src.cw, src.ox);
            let ry = tap(fy, src.ch, src.oy);
            let top = row_of(plan.src, plan.sstride, ry.lo);
            let bot = row_of(plan.src, plan.sstride, ry.hi);
            blend_px(px, sample_taps(top, bot, cx, ry.t), plan.opacity);
        }
    }
}

/// Row `y` of a packed buffer with `stride` bytes per row; empty when out of range.
fn row_of(buf: &[u8], stride: usize, y: usize) -> &[u8] {
    buf.get(y * stride..(y + 1) * stride).unwrap_or(&[])
}

/// Bilinear resize of a whole frame to `w` x `h` (each >= 1).
pub fn resize(frame: &Frame, w: u32, h: u32) -> Frame {
    let w = w.max(1);
    let h = h.max(1);
    if !is_valid(frame) {
        return solid_frame(w, h, [0, 0, 0, 255]);
    }
    let fw = frame.width as usize;
    let fh = frame.height as usize;
    let scale_x = f64::from(frame.width) / f64::from(w);
    let scale_y = f64::from(frame.height) / f64::from(h);
    let cols: Vec<Tap> = (0..w as usize)
        .map(|x| tap((x as f64 + 0.5) * scale_x, fw, 0))
        .collect();
    let rows: Vec<Tap> = (0..h as usize)
        .map(|y| tap((y as f64 + 0.5) * scale_y, fh, 0))
        .collect();
    let row_len = w as usize * 4;
    let fstride = fw * 4;
    let mut out = Frame {
        width: w,
        height: h,
        bgra: vec![0u8; row_len * h as usize],
    };
    let src = &frame.bgra;
    run_bands(&mut out.bgra, row_len, &rows, |band, rs| {
        for (row, ry) in band.chunks_exact_mut(row_len).zip(rs) {
            let top = row_of(src, fstride, ry.lo);
            let bot = row_of(src, fstride, ry.hi);
            for (px, &cx) in row.as_chunks_mut::<4>().0.iter_mut().zip(&cols) {
                store(px, sample_taps(top, bot, cx, ry.t));
            }
        }
    });
    out
}

/// Composes layers (bottom first) onto a canvas filled with `background`. Each layer is
/// cropped, rotated by quarter turns, flipped, scaled to its [`DestRect`] (bilinear), then
/// alpha-blended with `opacity * pixel alpha`, clipped to the canvas. The result is opaque
/// (alpha 255) wherever the background is opaque.
pub fn compose(canvas_w: u32, canvas_h: u32, background: Rgba, layers: &[LayerInput<'_>]) -> Frame {
    let mut out = solid_frame(
        canvas_w,
        canvas_h,
        [background.b, background.g, background.r, background.a],
    );
    for layer in layers {
        blend_layer(&mut out, layer);
    }
    out
}

/// Blends one layer onto `out` (see [`compose`]). Skips zero-opacity and empty layers.
fn blend_layer(out: &mut Frame, layer: &LayerInput<'_>) {
    let opacity = finite_or(layer.opacity, 0.0).clamp(0.0, 1.0) as f32;
    if opacity <= 0.0 || !is_valid(layer.frame) {
        return;
    }
    let t = layer.transform;
    // dest_rect applies the crop itself, so it takes the uncropped source size.
    let dr = dest_rect(
        layer.frame.width,
        layer.frame.height,
        out.width,
        out.height,
        t,
    );
    if !(dr.w > 0.0 && dr.h > 0.0) {
        return;
    }
    // Integer crop used for sampling. Same fractions as dest_rect, rounded down to pixels.
    let (ox, cw) = crop_span(layer.frame.width, t.crop.left, t.crop.right);
    let (oy, ch) = crop_span(layer.frame.height, t.crop.top, t.crop.bottom);
    let rot = t.rotate_90 % 4;
    let (ow, oh) = if rot % 2 == 1 {
        (ch as f64, cw as f64)
    } else {
        (cw as f64, ch as f64)
    };
    let src = Source {
        ox,
        oy,
        cw,
        ch,
        rot,
        flip_h: t.flip_h,
        flip_v: t.flip_v,
    };
    let (canvas_w, canvas_h) = (out.width, out.height);
    let (Some((x0, x1)), Some((y0, y1))) = (span(dr.x, dr.w, canvas_w), span(dr.y, dr.h, canvas_h))
    else {
        return;
    };

    // Oriented-source coordinates for every canvas column and row (pixel centres).
    let us: Vec<f64> = (x0..x1)
        .map(|x| (x as f64 + 0.5 - dr.x) / dr.w * ow)
        .collect();
    let vs: Vec<f64> = (y0..y1)
        .map(|y| (y as f64 + 0.5 - dr.y) / dr.h * oh)
        .collect();

    // 1:1, unrotated, pixel-aligned: nearest sampling is exact, no filtering needed.
    let fast = rot == 0
        && (dr.w - ow).abs() < 1e-6
        && (dr.h - oh).abs() < 1e-6
        && is_integral(dr.x)
        && is_integral(dr.y);

    let cs = canvas_w as usize * 4;
    let Some(region) = out.bgra.get_mut(y0 * cs..y1 * cs) else {
        return;
    };
    let plan = Plan {
        src: &layer.frame.bgra,
        sstride: layer.frame.width as usize * 4,
        opacity,
        fast,
    };

    if rot == 0 {
        // Unrotated: a source column depends only on the canvas column (and a row only on the
        // row), so the taps are computed once per layer.
        let cols: Vec<Tap> = us.iter().map(|&u| tap(src.map(u, 0.0).0, cw, ox)).collect();
        let rows: Vec<Tap> = vs.iter().map(|&v| tap(src.map(0.0, v).1, ch, oy)).collect();
        run_bands(region, cs, &rows, |band, rs| {
            blend_axis(band, cs, x0, &cols, rs, &plan);
        });
    } else {
        run_bands(region, cs, &vs, |band, vr| {
            blend_general(band, cs, x0, &us, vr, &src, &plan);
        });
    }
}

/// Cross-dissolve: `a * (1 - t) + b * t` per channel, frames must be the same size (else `a`).
pub fn dissolve(a: &Frame, b: &Frame, t: f64) -> Frame {
    if a.width != b.width || a.height != b.height || a.bgra.len() != b.bgra.len() {
        return a.clone();
    }
    let t = frac(t) as f32;
    let bgra = a
        .bgra
        .iter()
        .zip(b.bgra.iter())
        .map(|(&x, &y)| to_u8(f32::from(x) * (1.0 - t) + f32::from(y) * t))
        .collect();
    Frame {
        width: a.width,
        height: a.height,
        bgra,
    }
}

/// Blends `frame` toward a solid colour: `t = 0` unchanged, `t = 1` solid.
pub fn dip_to_color(frame: &Frame, color: Rgba, t: f64) -> Frame {
    let t = frac(t) as f32;
    let (tr, tg, tb) = (f32::from(color.r), f32::from(color.g), f32::from(color.b));
    let mut out = frame.clone();
    for px in out.bgra.as_chunks_mut::<4>().0 {
        let [b, g, r, _] = px;
        *b = to_u8(f32::from(*b) * (1.0 - t) + tb * t);
        *g = to_u8(f32::from(*g) * (1.0 - t) + tg * t);
        *r = to_u8(f32::from(*r) * (1.0 - t) + tr * t);
    }
    out
}

/// A background for fit-mode clips: `frame` scaled to cover `w` x `h` (fill) then blurred with a
/// box blur of radius about `w / 40`, darkened by 40 %. The blur is computed at 1/8 resolution
/// (the radius scales with it, so the look is the same) and bilinearly upscaled, which is far
/// cheaper and invisible on a blurred image.
pub fn blurred_cover(frame: &Frame, w: u32, h: u32) -> Frame {
    let w = w.max(1);
    let h = h.max(1);
    if !is_valid(frame) {
        return solid_frame(w, h, [0, 0, 0, 255]);
    }
    let sw = w.div_ceil(8).max(1);
    let sh = h.div_ceil(8).max(1);
    if (sw, sh) == (w, h) {
        return blurred_cover_direct(frame, w, h);
    }
    resize(&blurred_cover_direct(frame, sw, sh), w, h)
}

/// [`blurred_cover`] computed at exactly `w` x `h` (no downscaling).
fn blurred_cover_direct(frame: &Frame, w: u32, h: u32) -> Frame {
    // Cover: scale uniformly until both sides reach the target, then centre-crop.
    let fw = f64::from(frame.width);
    let fh = f64::from(frame.height);
    let s = (f64::from(w) / fw).max(f64::from(h) / fh);
    let cover_w = ((fw * s).ceil() as u32).max(w);
    let cover_h = ((fh * s).ceil() as u32).max(h);
    let covered = resize(frame, cover_w, cover_h);
    let x0 = ((cover_w - w) / 2) as usize * 4;
    let y0 = ((cover_h - h) / 2) as usize;
    let cover_stride = cover_w as usize * 4;
    let row_len = w as usize * 4;
    let mut cropped = Vec::with_capacity(row_len * h as usize);
    for row in covered
        .bgra
        .chunks_exact(cover_stride)
        .skip(y0)
        .take(h as usize)
    {
        if let Some(seg) = row.get(x0..x0 + row_len) {
            cropped.extend_from_slice(seg);
        }
    }
    if cropped.len() != row_len * h as usize {
        return solid_frame(w, h, [0, 0, 0, 255]);
    }

    let radius = ((w / 40).max(1)) as usize;
    let mut bgra = box_blur(&cropped, w as usize, h as usize, radius);
    for px in bgra.as_chunks_mut::<4>().0 {
        let [b, g, r, _] = px;
        *b = to_u8(f32::from(*b) * 0.6);
        *g = to_u8(f32::from(*g) * 0.6);
        *r = to_u8(f32::from(*r) * 0.6);
    }
    Frame {
        width: w,
        height: h,
        bgra,
    }
}

/// Clamps a possibly out-of-range index into `0..n` (edge replication). `n` must be >= 1.
fn clamp_index(i: isize, n: usize) -> usize {
    i.clamp(0, n as isize - 1) as usize
}

/// Pixel `x` of a row as u32 channels, zero when out of range.
fn px_u32(row: &[u8], x: usize) -> [u32; 4] {
    match row.get(x * 4..x * 4 + 4) {
        Some(&[b, g, r, a]) => [u32::from(b), u32::from(g), u32::from(r), u32::from(a)],
        _ => [0; 4],
    }
}

/// Two-pass separable box blur of radius `r` (window `2r + 1`, edges replicated) over a packed
/// `w` x `h` BGRA buffer. Running sums make it O(pixels). `w`, `h`, `r` must be >= 1 and
/// `src.len() == w * h * 4`.
fn box_blur(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let stride = w * 4;
    let div = (2 * r + 1) as u32;
    let half = div / 2;
    let ri = r as isize;

    // Horizontal pass: sliding window along each row.
    let mut tmp = vec![0u8; stride * h];
    for (row_in, row_out) in src.chunks_exact(stride).zip(tmp.chunks_exact_mut(stride)) {
        let mut acc = [0u32; 4];
        for k in 0..=2 * r {
            let p = px_u32(row_in, clamp_index(k as isize - ri, w));
            for (a, v) in acc.iter_mut().zip(p) {
                *a += v;
            }
        }
        for (x, out_px) in row_out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let xi = x as isize;
            for (o, a) in out_px.iter_mut().zip(acc) {
                *o = ((a + half) / div) as u8;
            }
            let add = px_u32(row_in, clamp_index(xi + ri + 1, w));
            let rem = px_u32(row_in, clamp_index(xi - ri, w));
            for ((a, ad), rm) in acc.iter_mut().zip(add).zip(rem) {
                *a = *a + ad - rm;
            }
        }
    }

    // Vertical pass: running sums of whole rows, one accumulator per byte column.
    let mut out = vec![0u8; stride * h];
    let mut acc = vec![0u32; stride];
    for k in 0..=2 * r {
        for (a, &v) in acc
            .iter_mut()
            .zip(row_of(&tmp, stride, clamp_index(k as isize - ri, h)))
        {
            *a += u32::from(v);
        }
    }
    for (y, out_row) in out.chunks_exact_mut(stride).enumerate() {
        let yi = y as isize;
        for (o, &a) in out_row.iter_mut().zip(acc.iter()) {
            *o = ((a + half) / div) as u8;
        }
        let add = row_of(&tmp, stride, clamp_index(yi + ri + 1, h));
        let rem = row_of(&tmp, stride, clamp_index(yi - ri, h));
        for ((a, &ad), &rm) in acc.iter_mut().zip(add).zip(rem) {
            *a = *a + u32::from(ad) - u32::from(rm);
        }
    }
    out
}
