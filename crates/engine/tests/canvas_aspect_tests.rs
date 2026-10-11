//! Tests for `clipcheck_engine::canvas`: aspect-preserving canvas sizing for
//! non-16:9 sources, the zero-input fallback, even dimensions, and the
//! `MAX_CANVAS_WIDTH` clamp.

use clipcheck_engine::canvas::{canvas_for_source, MAX_CANVAS_WIDTH};

#[test]
fn sixteen_nine_1080() {
    assert_eq!(canvas_for_source(1920, 1080, 1080), (1920, 1080));
}

#[test]
fn sixteen_nine_480_is_854() {
    // 480 * 1920 / 1080 = 853.33 -> 853 -> made even -> 854.
    assert_eq!(canvas_for_source(1920, 1080, 480), (854, 480));
}

#[test]
fn ultrawide_1440() {
    assert_eq!(canvas_for_source(3440, 1440, 1440), (3440, 1440));
}

#[test]
fn ultrawide_1080() {
    // 1080 * 3440 / 1440 = 2580.0
    assert_eq!(canvas_for_source(3440, 1440, 1080), (2580, 1080));
}

#[test]
fn portrait_1080x1920_at_1440() {
    // 1440 * 1080 / 1920 = 810.0
    assert_eq!(canvas_for_source(1080, 1920, 1440), (810, 1440));
}

#[test]
fn clamp_32_9() {
    // Width 1440 * 5120 / 1440 = 5120 > 3840 -> width 3840,
    // height 3840 * 1440 / 5120 = 1080.
    assert_eq!(canvas_for_source(5120, 1440, 1440), (3840, 1080));
}

#[test]
fn zero_src_falls_back_to_16_9() {
    assert_eq!(canvas_for_source(0, 0, 1080), (1920, 1080));
}

#[test]
fn zero_height_gives_min_size() {
    let (w, h) = canvas_for_source(1920, 1080, 0);
    assert!(w >= 2, "width {w} must be at least 2");
    assert!(h >= 2, "height {h} must be at least 2");
    assert_eq!(w % 2, 0, "width {w} must be even");
    assert_eq!(h % 2, 0, "height {h} must be even");
}

#[test]
fn always_even() {
    let sources = [(3441u32, 1441u32), (1601, 901), (2561, 1441)];
    let heights = [720u32, 1080, 1440];
    for (src_w, src_h) in sources {
        for height in heights {
            let (w, h) = canvas_for_source(src_w, src_h, height);
            assert_eq!(
                w % 2,
                0,
                "width {w} not even for src {src_w}x{src_h} at height {height}"
            );
            assert_eq!(
                h % 2,
                0,
                "height {h} not even for src {src_w}x{src_h} at height {height}"
            );
        }
    }
}

#[test]
fn never_exceeds_max_width() {
    let sources = [(5120u32, 1440u32), (7680, 2160), (10000, 1000)];
    for (src_w, src_h) in sources {
        let (w, _h) = canvas_for_source(src_w, src_h, 2160);
        assert!(
            w <= MAX_CANVAS_WIDTH,
            "width {w} exceeds MAX_CANVAS_WIDTH {MAX_CANVAS_WIDTH} for src {src_w}x{src_h}"
        );
    }
}
