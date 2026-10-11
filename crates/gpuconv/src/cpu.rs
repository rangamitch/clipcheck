//! Software BGRA -> NV12 with bilinear scaling and letterboxing (worker G1).

use crate::ConvError;

/// Destination rect `(x, y, w, h)` of an `sw x sh` source fitted inside `cw x ch`.
///
/// Aspect is preserved. The fitted size is rounded down to even (minimum 2, never more
/// than the canvas), and the offset is centred and rounded down to even.
pub fn letterbox_rect(sw: u32, sh: u32, cw: u32, ch: u32) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 || cw == 0 || ch == 0 {
        return (0, 0, 0, 0);
    }
    let (swu, shu, cwu, chu) = (u64::from(sw), u64::from(sh), u64::from(cw), u64::from(ch));
    // Source narrower than (or same aspect as) the canvas: fit the height.
    let (w, h) = if swu * chu <= cwu * shu {
        ((swu * chu + shu / 2) / shu, chu)
    } else {
        (cwu, (shu * cwu + swu / 2) / swu)
    };
    let w = fit_even(w, cw);
    let h = fit_even(h, ch);
    let x = ((cw - w) / 2) & !1;
    let y = ((ch - h) / 2) & !1;
    (x, y, w, h)
}

/// Rounds `v` down to even, at least 2, and at most `max`.
fn fit_even(v: u64, max: u32) -> u32 {
    (v & !1).max(2).min(u64::from(max)) as u32
}

/// Converts tightly packed BGRA `sw x sh` into NV12 `cw x ch`, letterboxed.
///
/// `out` is resized to `cw * ch * 3 / 2` and fully overwritten: the Y plane, then the
/// interleaved UV plane. The canvas starts black (Y=16, U=V=128) and the letterboxed
/// source is bilinearly sampled at pixel centres.
pub fn bgra_to_nv12(
    src: &[u8],
    sw: u32,
    sh: u32,
    cw: u32,
    ch: u32,
    out: &mut Vec<u8>,
) -> Result<(), ConvError> {
    if sw == 0 || sh == 0 {
        return Err(bad(format!("source size {sw}x{sh} has a zero side")));
    }
    if cw == 0 || ch == 0 || !cw.is_multiple_of(2) || !ch.is_multiple_of(2) {
        return Err(bad(format!("canvas {cw}x{ch} must be non-zero and even")));
    }
    let expected = (sw as usize)
        .checked_mul(sh as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| bad(format!("source size {sw}x{sh} overflows")))?;
    if src.len() != expected {
        return Err(bad(format!(
            "source buffer is {} bytes, expected {expected} for {sw}x{sh} BGRA",
            src.len()
        )));
    }

    let cwu = cw as usize;
    let chu = ch as usize;
    let luma_len = cwu * chu;
    out.clear();
    out.resize(luma_len + luma_len / 2, 128);
    out[..luma_len].fill(16);

    let (x, y, w, h) = letterbox_rect(sw, sh, cw, ch);
    if w == 0 || h == 0 {
        return Ok(());
    }
    let (xu, yu, wu, hu) = (x as usize, y as usize, w as usize, h as usize);
    let (swu, shu) = (sw as usize, sh as usize);
    let stride = swu * 4;

    // Per-column and per-row sample positions, computed once per call.
    let cols: Vec<(usize, usize, f32)> = (0..wu).map(|d| axis(d, wu, swu)).collect();
    let rows: Vec<(usize, usize, f32)> = (0..hu).map(|d| axis(d, hu, shu)).collect();

    let mut rgb0 = vec![[0f32; 3]; wu];
    let mut rgb1 = vec![[0f32; 3]; wu];

    // Destination rows are processed in pairs so each 2x2 chroma block is complete.
    // hu, wu, xu and yu are all even, so pairs never straddle the rect edge.
    for pair in (0..hu).step_by(2) {
        let (r0a, r1a, fya) = rows[pair];
        bilinear_row(src, stride, r0a, r1a, fya, &cols, &mut rgb0);
        let (r0b, r1b, fyb) = rows[pair + 1];
        bilinear_row(src, stride, r0b, r1b, fyb, &cols, &mut rgb1);

        let abs_y = yu + pair;
        let cy = abs_y / 2;
        for dx in (0..wu).step_by(2) {
            let abs_x = xu + dx;
            let quad = [rgb0[dx], rgb0[dx + 1], rgb1[dx], rgb1[dx + 1]];
            let positions = [
                (abs_y, abs_x),
                (abs_y, abs_x + 1),
                (abs_y + 1, abs_x),
                (abs_y + 1, abs_x + 1),
            ];
            for (p, (py, px)) in quad.iter().zip(positions) {
                out[py * cwu + px] = luma(*p);
            }
            let mut sum = [0f32; 3];
            for p in &quad {
                sum[0] += p[0];
                sum[1] += p[1];
                sum[2] += p[2];
            }
            let avg = [sum[0] / 4.0, sum[1] / 4.0, sum[2] / 4.0];
            let idx = luma_len + cy * cwu + (abs_x / 2) * 2;
            out[idx] = chroma_u(avg);
            out[idx + 1] = chroma_v(avg);
        }
    }
    Ok(())
}

fn bad(message: String) -> ConvError {
    ConvError::BadInput(message)
}

/// Source sample position for destination index `dst` of `dst_len`, from `src_len`.
/// Returns `(index0, index1, fraction)` for a bilinear pair, clamped to the source.
fn axis(dst: usize, dst_len: usize, src_len: usize) -> (usize, usize, f32) {
    let max = (src_len - 1) as f64;
    let s = ((dst as f64 + 0.5) * src_len as f64 / dst_len as f64 - 0.5).clamp(0.0, max);
    let i0 = s.floor() as usize;
    let i1 = (i0 + 1).min(src_len - 1);
    (i0, i1, (s - i0 as f64) as f32)
}

/// Bilinear sample of one destination row into `out` as `[R, G, B]` (0..=255 floats).
/// Source BGRA byte order: B at +0, G at +1, R at +2; alpha is ignored.
fn bilinear_row(
    src: &[u8],
    stride: usize,
    r0: usize,
    r1: usize,
    fy: f32,
    cols: &[(usize, usize, f32)],
    out: &mut [[f32; 3]],
) {
    let row0 = &src[r0 * stride..(r0 + 1) * stride];
    let row1 = &src[r1 * stride..(r1 + 1) * stride];
    for (o, &(x0, x1, fx)) in out.iter_mut().zip(cols) {
        let a = x0 * 4;
        let b = x1 * 4;
        let channel = |off: usize| {
            let t0 = f32::from(row0[a + off]);
            let t1 = f32::from(row0[b + off]);
            let b0 = f32::from(row1[a + off]);
            let b1 = f32::from(row1[b + off]);
            let top = t0 + (t1 - t0) * fx;
            let bottom = b0 + (b1 - b0) * fx;
            top + (bottom - top) * fy
        };
        *o = [channel(2), channel(1), channel(0)];
    }
}

fn quantise(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn luma(p: [f32; 3]) -> u8 {
    quantise(16.0 + 0.1826 * p[0] + 0.6142 * p[1] + 0.0620 * p[2])
}

fn chroma_u(p: [f32; 3]) -> u8 {
    quantise(128.0 - 0.1006 * p[0] - 0.3386 * p[1] + 0.4392 * p[2])
}

fn chroma_v(p: [f32; 3]) -> u8 {
    quantise(128.0 + 0.4392 * p[0] - 0.3989 * p[1] - 0.0403 * p[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_same_aspect_fills_canvas() {
        assert_eq!(letterbox_rect(1920, 1080, 1280, 720), (0, 0, 1280, 720));
        assert_eq!(letterbox_rect(640, 480, 1280, 960), (0, 0, 1280, 960));
    }

    #[test]
    fn letterbox_portrait_is_centred_pillarbox() {
        assert_eq!(letterbox_rect(1080, 1920, 1280, 720), (438, 0, 404, 720));
    }

    #[test]
    fn letterbox_zero_input_is_empty() {
        assert_eq!(letterbox_rect(0, 1080, 1280, 720), (0, 0, 0, 0));
        assert_eq!(letterbox_rect(1920, 1080, 0, 720), (0, 0, 0, 0));
    }

    #[test]
    fn letterbox_tiny_source_is_at_least_two_pixels() {
        assert_eq!(letterbox_rect(1, 1, 2, 2), (0, 0, 2, 2));
    }

    #[test]
    fn white_converts_to_video_white() {
        let src = [255u8; 2 * 2 * 4];
        let mut out = Vec::new();
        bgra_to_nv12(&src, 2, 2, 2, 2, &mut out).unwrap();
        assert_eq!(out.len(), 6);
        assert_eq!(&out[..4], &[235, 235, 235, 235]);
        assert_eq!(&out[4..], &[128, 128]);
    }

    #[test]
    fn black_converts_to_video_black() {
        let src: Vec<u8> = (0..16).flat_map(|_| [0u8, 0, 0, 255]).collect();
        let mut out = Vec::new();
        bgra_to_nv12(&src, 4, 4, 4, 4, &mut out).unwrap();
        assert!(out[..16].iter().all(|&v| v == 16));
        assert!(out[16..].iter().all(|&v| v == 128));
    }

    #[test]
    fn letterbox_margins_are_black() {
        // 2x2 white onto 4x2: the source lands at x=0..2, columns 2..4 stay black.
        let src = [255u8; 2 * 2 * 4];
        let mut out = Vec::new();
        bgra_to_nv12(&src, 2, 2, 4, 2, &mut out).unwrap();
        let (x, _, w, _) = letterbox_rect(2, 2, 4, 2);
        assert_eq!((x, w), (0, 2));
        assert_eq!(out[0], 235);
        assert_eq!(out[2], 16);
        assert_eq!(out[3], 16);
    }

    #[test]
    fn rejects_bad_inputs() {
        let mut out = Vec::new();
        assert!(matches!(
            bgra_to_nv12(&[], 0, 1, 2, 2, &mut out),
            Err(ConvError::BadInput(_))
        ));
        assert!(matches!(
            bgra_to_nv12(&[0u8; 4], 1, 1, 3, 2, &mut out),
            Err(ConvError::BadInput(_))
        ));
        assert!(matches!(
            bgra_to_nv12(&[0u8; 7], 1, 2, 2, 2, &mut out),
            Err(ConvError::BadInput(_))
        ));
    }
}
