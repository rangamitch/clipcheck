//! Encoder canvas size that follows the captured window's aspect ratio (D54).
//!
//! The canvas height comes from the quality preset or the custom resolution. Its width
//! comes from the captured source, so a 3440x1440 window records as 3440x1440 instead of
//! being letterboxed into 16:9. Pure arithmetic only: no Windows calls, no I/O.

/// Widest canvas the encoder is asked for (H.264 and hardware encoders reject wider frames
/// in practice). A wider source keeps its aspect: the width is clamped and the height shrinks.
pub const MAX_CANVAS_WIDTH: u32 = 3840;

/// Canvas size for a source of `src_w` x `src_h` pixels at a target `height`.
///
/// Rules, in order:
/// - Any zero input (`src_w`, `src_h` or `height`) gives the 16:9 canvas for `height`.
/// - `width = round(height * src_w / src_h)`, halves rounded up, then made even.
/// - If the width exceeds [`MAX_CANVAS_WIDTH`], the width is clamped there and the height is
///   scaled to keep the source aspect (rounded, then made even).
/// - Both dimensions are at least 2 and even (NV12 needs even sizes).
///
/// 16:9 sources give the same sizes as the old fixed rule: 1080 -> 1920x1080, 480 -> 854x480.
pub fn canvas_for_source(src_w: u32, src_h: u32, height: u32) -> (u32, u32) {
    let (src_w, src_h) = if src_w == 0 || src_h == 0 || height == 0 {
        (16, 9)
    } else {
        (src_w, src_h)
    };
    let height = if height == 0 { 2 } else { height };
    let (sw, sh, h) = (u64::from(src_w), u64::from(src_h), u64::from(height));
    let mut w = (h * sw + sh / 2) / sh;
    let mut out_h = h;
    if w > u64::from(MAX_CANVAS_WIDTH) {
        w = u64::from(MAX_CANVAS_WIDTH);
        out_h = (w * sh + sw / 2) / sw;
    }
    w += w & 1;
    out_h = (out_h & !1).max(2);
    let width = (w as u32 & !1).max(2);
    (width, out_h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_nine_sources_keep_the_old_sizes() {
        assert_eq!(canvas_for_source(1920, 1080, 1080), (1920, 1080));
        assert_eq!(canvas_for_source(2560, 1440, 1440), (2560, 1440));
        assert_eq!(canvas_for_source(1920, 1080, 480), (854, 480));
    }

    #[test]
    fn ultrawide_follows_the_source() {
        assert_eq!(canvas_for_source(3440, 1440, 1440), (3440, 1440));
    }
}
