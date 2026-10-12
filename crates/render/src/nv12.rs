//! BGRA to NV12 conversion for the encoder input (BT.709, limited range, matching the GPU path).

use crate::frame::Frame;

/// Frames shorter than this many rows are converted on the calling thread.
const MIN_PARALLEL_HEIGHT: usize = 128;
/// Upper bound on worker threads for one conversion.
const MAX_THREADS: usize = 8;

/// Converts a BGRA frame to NV12 (BT.709, limited range) in `out`. Width and height are even.
///
/// Well-formed even-sized frames take the banded fast path. Odd-sized frames and frames whose
/// `bgra` buffer is shorter than `width * height * 4` take the bounds-checked slow path, which
/// never panics. Both paths produce identical bytes.
pub fn bgra_to_nv12(frame: &Frame, out: &mut Vec<u8>) {
    out.clear();
    let w = frame.width as usize;
    let h = frame.height as usize;
    let Some(n) = w.checked_mul(h) else {
        return;
    };
    out.resize(n + n / 2, 0);
    let (luma, chroma) = out.split_at_mut(n);
    let src = n.checked_mul(4).and_then(|len| frame.bgra.get(..len));
    match src {
        Some(src) if w > 0 && h > 0 && w.is_multiple_of(2) && h.is_multiple_of(2) => {
            convert_fast(src, w, luma, chroma);
        }
        _ => convert_slow(frame, w, h, luma, chroma),
    }
}

/// Fast path: `src` is exactly `w * h * 4` bytes, `w` and `h` are even and non-zero,
/// `luma` is `w * h` bytes and `chroma` is `w * h / 2` bytes.
fn convert_fast(src: &[u8], w: usize, luma: &mut [u8], chroma: &mut [u8]) {
    let block_rows = luma.len() / (2 * w);
    let threads = band_threads(block_rows);
    if threads <= 1 {
        convert_band(src, luma, chroma, w);
        return;
    }
    let band_rows = block_rows.div_ceil(threads);
    std::thread::scope(|scope| {
        let bands = src
            .chunks(band_rows * 2 * w * 4)
            .zip(luma.chunks_mut(band_rows * 2 * w))
            .zip(chroma.chunks_mut(band_rows * w));
        for ((src_band, luma_band), chroma_band) in bands {
            scope.spawn(move || convert_band(src_band, luma_band, chroma_band, w));
        }
    });
}

/// Number of worker threads for `block_rows` pairs of rows.
fn band_threads(block_rows: usize) -> usize {
    if block_rows * 2 < MIN_PARALLEL_HEIGHT {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_THREADS)
}

/// Converts a band of whole 2-row groups. `src` holds `2 * w * 4` bytes per group, `luma`
/// `2 * w` bytes per group and `chroma` `w` bytes per group.
fn convert_band(src: &[u8], luma: &mut [u8], chroma: &mut [u8], w: usize) {
    let groups = src
        .chunks_exact(2 * w * 4)
        .zip(luma.chunks_exact_mut(2 * w))
        .zip(chroma.chunks_exact_mut(w));
    for ((pair, luma_pair), chroma_row) in groups {
        let (top, bottom) = pair.split_at(w * 4);
        let (luma_top, luma_bottom) = luma_pair.split_at_mut(w);
        let (top_px, _) = top.as_chunks::<8>();
        let (bottom_px, _) = bottom.as_chunks::<8>();
        let (luma_top_px, _) = luma_top.as_chunks_mut::<2>();
        let (luma_bottom_px, _) = luma_bottom.as_chunks_mut::<2>();
        let (chroma_px, _) = chroma_row.as_chunks_mut::<2>();
        let blocks = top_px
            .iter()
            .zip(bottom_px.iter())
            .zip(luma_top_px.iter_mut())
            .zip(luma_bottom_px.iter_mut())
            .zip(chroma_px.iter_mut());
        for ((((t, b), ly0), ly1), uv) in blocks {
            // Each 8-byte chunk holds two BGRA pixels: [B, G, R, A, B, G, R, A].
            let [b00, g00, r00, _, b01, g01, r01, _] = *t;
            let [b10, g10, r10, _, b11, g11, r11, _] = *b;
            let [y00, y01] = ly0;
            let [y10, y11] = ly1;
            let [cu, cv] = uv;
            let (ls, u, v) = block_values([
                (b00, g00, r00),
                (b01, g01, r01),
                (b10, g10, r10),
                (b11, g11, r11),
            ]);
            let [l00, l01, l10, l11] = ls;
            *y00 = l00;
            *y01 = l01;
            *y10 = l10;
            *y11 = l11;
            *cu = u;
            *cv = v;
        }
    }
}

/// Luma of one pixel (BT.709, limited range): Y = 16 + 219 * (0.2126 R + 0.7152 G + 0.0722 B) / 255,
/// with the coefficients scaled by 256 (47 + 157 + 16 = 220, which is 219 * 256 / 255 rounded up).
#[inline]
fn luma_of(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(0, 255) as u8
}

/// Averaged chroma (U, V) of one 2x2 block mean (BT.709, limited range). The coefficients are the
/// BT.709 Pb/Pr matrix scaled to the 224-level chroma range and by 256: U uses -26, -87, +112
/// (sum -1, so white reads U = 127, within the +-1 tolerance) and V uses +112, -102, -10 (sum 0).
#[inline]
fn uv_of(r: i32, g: i32, b: i32) -> (u8, u8) {
    let u = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
    let v = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
    (u, v)
}

/// Luma of the four pixels of a 2x2 block (in order top-left, top-right, bottom-left,
/// bottom-right, each as `(b, g, r)`), plus the block's averaged U and V.
#[inline]
fn block_values(quad: [(u8, u8, u8); 4]) -> ([u8; 4], u8, u8) {
    let mut lumas = [0u8; 4];
    let (mut sum_r, mut sum_g, mut sum_b) = (0i32, 0i32, 0i32);
    for (dst, &(b, g, r)) in lumas.iter_mut().zip(quad.iter()) {
        let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
        sum_r += r;
        sum_g += g;
        sum_b += b;
        *dst = luma_of(r, g, b);
    }
    let (u, v) = uv_of(sum_r / 4, sum_g / 4, sum_b / 4);
    (lumas, u, v)
}

/// Slow path for odd sizes and malformed buffers. Pixels missing from `frame.bgra` are skipped
/// (their luma stays zero and they do not contribute to the chroma average), and luma/chroma
/// outside the even-sized area stay zero.
fn convert_slow(frame: &Frame, w: usize, h: usize, luma: &mut [u8], chroma: &mut [u8]) {
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let mut sum_r = 0i32;
            let mut sum_g = 0i32;
            let mut sum_b = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let x = bx * 2 + dx;
                    let y = by * 2 + dy;
                    let Some([b, g, r, _]) = frame.pixel(x as u32, y as u32) else {
                        continue;
                    };
                    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
                    sum_r += r;
                    sum_g += g;
                    sum_b += b;
                    if let Some(dst) = luma.get_mut(y * w + x) {
                        *dst = luma_of(r, g, b);
                    }
                }
            }
            let (u, v) = uv_of(sum_r / 4, sum_g / 4, sum_b / 4);
            let at = by * w + bx * 2;
            if let Some(dst) = chroma.get_mut(at) {
                *dst = u;
            }
            if let Some(dst) = chroma.get_mut(at + 1) {
                *dst = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straightforward per-pixel implementation (the original code), used as the oracle.
    fn reference_nv12(frame: &Frame) -> Vec<u8> {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let mut out = vec![0u8; w * h + w * h / 2];
        let (luma, chroma) = out.split_at_mut(w * h);
        for by in 0..h / 2 {
            for bx in 0..w / 2 {
                let mut sum_r = 0i32;
                let mut sum_g = 0i32;
                let mut sum_b = 0i32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let x = bx * 2 + dx;
                        let y = by * 2 + dy;
                        let Some([b, g, r, _]) = frame.pixel(x as u32, y as u32) else {
                            continue;
                        };
                        let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
                        sum_r += r;
                        sum_g += g;
                        sum_b += b;
                        if let Some(dst) = luma.get_mut(y * w + x) {
                            *dst =
                                (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
                        }
                    }
                }
                let (u, v) = uv_of(sum_r / 4, sum_g / 4, sum_b / 4);
                let at = by * w + bx * 2;
                if let Some(dst) = chroma.get_mut(at) {
                    *dst = u;
                }
                if let Some(dst) = chroma.get_mut(at + 1) {
                    *dst = v;
                }
            }
        }
        out
    }

    /// Deterministic pseudo-random BGRA frame (LCG bytes).
    fn pseudo_random_frame(width: u32, height: u32, seed: u32) -> Frame {
        let len = width as usize * height as usize * 4;
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut bgra = Vec::with_capacity(len);
        for _ in 0..len {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bgra.push((state >> 24) as u8);
        }
        Frame {
            width,
            height,
            bgra,
        }
    }

    fn assert_matches_reference(frame: &Frame) {
        let mut out = Vec::new();
        bgra_to_nv12(frame, &mut out);
        assert_eq!(out, reference_nv12(frame));
    }

    #[test]
    fn fast_path_matches_reference_64x34() {
        assert_matches_reference(&pseudo_random_frame(64, 34, 1));
    }

    #[test]
    fn fast_path_matches_reference_2x2() {
        assert_matches_reference(&pseudo_random_frame(2, 2, 7));
    }

    /// Float BT.709 limited-range YUV of one BGRA-ordered (r, g, b) colour.
    fn float_bt709(r: u8, g: u8, b: u8) -> [f64; 3] {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let yy = 16.0 + 219.0 * y / 255.0;
        let u = 128.0 + 224.0 * ((b - y) / 255.0) / 1.8556;
        let v = 128.0 + 224.0 * ((r - y) / 255.0) / 1.5748;
        [yy, u, v]
    }

    /// Converts a 2x2 frame of one solid colour and returns its (Y, U, V) bytes.
    fn solid_yuv(r: u8, g: u8, b: u8) -> [u8; 3] {
        let frame = Frame {
            width: 2,
            height: 2,
            bgra: [b, g, r, 255].repeat(4),
        };
        let mut out = Vec::new();
        bgra_to_nv12(&frame, &mut out);
        // Y plane is 4 bytes, then one interleaved U V pair.
        [out[0], out[4], out[5]]
    }

    #[test]
    fn bt709_solid_colours_match_float_within_one() {
        let colours: [(&str, [u8; 3]); 6] = [
            ("black", [0, 0, 0]),
            ("white", [255, 255, 255]),
            ("red", [255, 0, 0]),
            ("green", [0, 255, 0]),
            ("blue", [0, 0, 255]),
            ("grey", [128, 128, 128]),
        ];
        for (name, [r, g, b]) in colours {
            let got = solid_yuv(r, g, b);
            let want = float_bt709(r, g, b);
            for (k, (&a, &e)) in got.iter().zip(want.iter()).enumerate() {
                let diff = (f64::from(a) - e.round()).abs();
                assert!(diff <= 1.0, "{name} channel {k}: got {a}, float {e:.2}");
            }
        }
    }

    #[test]
    fn bt709_differs_from_bt601_for_pure_green() {
        // BT.601 would give Y = 149 for pure green; BT.709 gives 172 (float 172.6, within one).
        assert_eq!(solid_yuv(0, 255, 0)[0], 172);
    }

    #[test]
    fn threaded_path_matches_reference() {
        // Tall enough to take the multi-threaded band split, with an uneven band count.
        assert_matches_reference(&pseudo_random_frame(32, 302, 3));
    }
}
