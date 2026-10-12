//! Public-API tests for `clipcheck_render::reference` (CPU compositor). Synthetic frames only.
//! Pixel tuples are returned by `Frame::pixel` as [b, g, r, a].

use clipcheck_render::frame::Frame;
use clipcheck_render::reference::{
    blurred_cover, compose, dest_rect, dip_to_color, dissolve, resize, DestRect, LayerInput,
};
use clipcheck_timeline::model::{Crop, FitMode, Rgba, Transform};

const RED: [u8; 4] = [0, 0, 255, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn rgba(r: u8, g: u8, b: u8) -> Rgba {
    Rgba { r, g, b, a: 255 }
}

fn tr(fit: FitMode) -> Transform {
    Transform {
        fit,
        ..Transform::default()
    }
}

fn layer<'a>(frame: &'a Frame, t: &'a Transform, opacity: f64) -> LayerInput<'a> {
    LayerInput {
        frame,
        transform: t,
        opacity,
    }
}

/// Writes one BGRA pixel into a frame (test helper; coordinates must be in bounds).
fn set_px(f: &mut Frame, x: u32, y: u32, px: [u8; 4]) {
    let i = ((y * f.width + x) * 4) as usize;
    f.bgra[i..i + 4].copy_from_slice(&px);
}

fn px(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    f.pixel(x, y).expect("pixel in bounds")
}

fn assert_rect(r: DestRect, x: f64, y: f64, w: f64, h: f64) {
    let tol = 1e-6;
    assert!((r.x - x).abs() < tol, "x: expected {x}, got {}", r.x);
    assert!((r.y - y).abs() < tol, "y: expected {y}, got {}", r.y);
    assert!((r.w - w).abs() < tol, "w: expected {w}, got {}", r.w);
    assert!((r.h - h).abs() < tol, "h: expected {h}, got {}", r.h);
}

fn is_zero_rect(r: DestRect) -> bool {
    r.w == 0.0 && r.h == 0.0
}

// ---------------------------------------------------------------- dest_rect

#[test]
fn dest_rect_fit_16x9_into_9x16_centred_bars() {
    // 1920x1080 into 1080x1920: scale 0.5625 -> 1080 x 607.5, centred vertically.
    let r = dest_rect(1920, 1080, 1080, 1920, &tr(FitMode::Fit));
    assert_rect(r, 0.0, (1920.0 - 607.5) / 2.0, 1080.0, 607.5);
}

#[test]
fn dest_rect_fit_same_aspect_fills_canvas() {
    let r = dest_rect(1920, 1080, 1920, 1080, &tr(FitMode::Fit));
    assert_rect(r, 0.0, 0.0, 1920.0, 1080.0);
}

#[test]
fn dest_rect_fit_into_square_centred() {
    // Scale 1000/1920; height 562.5 centred in 1000.
    let r = dest_rect(1920, 1080, 1000, 1000, &tr(FitMode::Fit));
    let s = 1000.0 / 1920.0;
    assert_rect(r, 0.0, (1000.0 - 1080.0 * s) / 2.0, 1000.0, 1080.0 * s);
}

#[test]
fn dest_rect_fill_16x9_into_9x16_overflows_sides() {
    let r = dest_rect(1920, 1080, 1080, 1920, &tr(FitMode::Fill));
    let s = 1920.0 / 1080.0;
    let w = 1920.0 * s;
    assert_rect(r, (1080.0 - w) / 2.0, 0.0, w, 1920.0);
    assert!(r.x < 0.0, "fill must overflow the canvas horizontally");
}

#[test]
fn dest_rect_stretch_is_canvas() {
    let r = dest_rect(640, 480, 1920, 1080, &tr(FitMode::Stretch));
    assert_rect(r, 0.0, 0.0, 1920.0, 1080.0);
}

#[test]
fn dest_rect_crop_horizontal_removes_sides() {
    let mut t = tr(FitMode::Fit);
    t.crop = Crop {
        left: 0.25,
        right: 0.25,
        top: 0.0,
        bottom: 0.0,
    };
    // Cropped source 960x1080 fits 1920x1080 at scale 1: centred horizontally.
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 480.0, 0.0, 960.0, 1080.0);
}

#[test]
fn dest_rect_crop_vertical_removes_top_bottom() {
    let mut t = tr(FitMode::Fit);
    t.crop = Crop {
        left: 0.0,
        right: 0.0,
        top: 0.25,
        bottom: 0.25,
    };
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 0.0, 270.0, 1920.0, 540.0);
}

#[test]
fn dest_rect_rotate_90_swaps_width_and_height() {
    // Oriented source is 1080x1920, which fits 1080x1920 exactly.
    let mut t = tr(FitMode::Fit);
    t.rotate_90 = 1;
    let r = dest_rect(1920, 1080, 1080, 1920, &t);
    assert_rect(r, 0.0, 0.0, 1080.0, 1920.0);
}

#[test]
fn dest_rect_rotate_270_swaps_width_and_height() {
    let mut t = tr(FitMode::Fit);
    t.rotate_90 = 3;
    let r = dest_rect(1920, 1080, 1080, 1920, &t);
    assert_rect(r, 0.0, 0.0, 1080.0, 1920.0);
}

#[test]
fn dest_rect_rotate_180_does_not_swap() {
    let mut t = tr(FitMode::Fit);
    t.rotate_90 = 2;
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 0.0, 0.0, 1920.0, 1080.0);
}

#[test]
fn dest_rect_custom_scale_half_centred() {
    let mut t = tr(FitMode::Custom);
    t.scale = 0.5;
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 480.0, 270.0, 960.0, 540.0);
}

#[test]
fn dest_rect_custom_pos_offsets_are_canvas_fractions() {
    let mut t = tr(FitMode::Custom);
    t.scale = 0.5;
    t.pos_x = 0.1;
    t.pos_y = -0.2;
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 480.0 + 192.0, 270.0 - 216.0, 960.0, 540.0);
}

#[test]
fn dest_rect_custom_scale_two_overflows_centred() {
    let mut t = tr(FitMode::Custom);
    t.scale = 2.0;
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, -960.0, -540.0, 3840.0, 2160.0);
}

#[test]
fn dest_rect_custom_bad_scale_is_zero_rect() {
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut t = tr(FitMode::Custom);
        t.scale = bad;
        assert!(
            is_zero_rect(dest_rect(1920, 1080, 1920, 1080, &t)),
            "scale {bad} must give the zero rectangle"
        );
    }
}

#[test]
fn dest_rect_custom_nonfinite_pos_treated_as_zero() {
    let mut t = tr(FitMode::Custom);
    t.scale = 0.5;
    t.pos_x = f64::NAN;
    t.pos_y = f64::INFINITY;
    let r = dest_rect(1920, 1080, 1920, 1080, &t);
    assert_rect(r, 480.0, 270.0, 960.0, 540.0);
}

#[test]
fn dest_rect_zero_source_is_zero_rect() {
    assert!(is_zero_rect(dest_rect(
        0,
        1080,
        1920,
        1080,
        &tr(FitMode::Fit)
    )));
    assert!(is_zero_rect(dest_rect(
        1920,
        0,
        1920,
        1080,
        &tr(FitMode::Stretch)
    )));
}

#[test]
fn dest_rect_zero_canvas_is_zero_rect() {
    assert!(is_zero_rect(dest_rect(
        1920,
        1080,
        0,
        1080,
        &tr(FitMode::Fit)
    )));
    assert!(is_zero_rect(dest_rect(
        1920,
        1080,
        1920,
        0,
        &tr(FitMode::Fill)
    )));
}

#[test]
fn dest_rect_fully_cropped_is_zero_rect() {
    let mut t = tr(FitMode::Fit);
    t.crop = Crop {
        left: 0.5,
        right: 0.5,
        top: 0.0,
        bottom: 0.0,
    };
    assert!(is_zero_rect(dest_rect(1920, 1080, 1920, 1080, &t)));
}

// ---------------------------------------------------------------- resize

#[test]
fn resize_identity_keeps_checker_pattern() {
    let mut f = Frame::solid(2, 2, 0, 0, 0, 255);
    set_px(&mut f, 1, 0, [255, 255, 255, 255]);
    set_px(&mut f, 0, 1, [255, 255, 255, 255]);
    let out = resize(&f, 2, 2);
    assert_eq!(out, f);
}

#[test]
fn resize_downscale_solid_keeps_colour() {
    let f = Frame::solid(16, 16, 10, 20, 30, 255);
    let out = resize(&f, 8, 8);
    assert_eq!((out.width, out.height), (8, 8));
    assert_eq!(out, Frame::solid(8, 8, 10, 20, 30, 255));
}

#[test]
fn resize_invalid_frame_is_black_without_panic() {
    let bad = Frame {
        width: 2,
        height: 2,
        bgra: vec![0; 3],
    };
    let out = resize(&bad, 4, 4);
    assert_eq!((out.width, out.height), (4, 4));
    assert_eq!(out, Frame::solid(4, 4, 0, 0, 0, 255));
}

// ---------------------------------------------------------------- compose

#[test]
fn compose_fit_leaves_background_bars() {
    // 100x100 red layer, Fit on a 200x100 canvas: 100x100 at x = 50, bars on both sides.
    let canvas_bg = rgba(10, 20, 30);
    let src = Frame::solid(100, 100, 0, 0, 255, 255);
    let t = tr(FitMode::Fit);
    let out = compose(200, 100, canvas_bg, &[layer(&src, &t, 1.0)]);
    let background = [30, 20, 10, 255];
    assert_eq!(px(&out, 0, 0), background, "corner is background");
    assert_eq!(px(&out, 10, 50), background, "left bar is background");
    assert_eq!(px(&out, 190, 50), background, "right bar is background");
    assert_eq!(px(&out, 100, 50), RED, "centre is the layer");
}

#[test]
fn compose_fill_covers_whole_canvas() {
    let src = Frame::solid(100, 100, 0, 0, 255, 255);
    let t = tr(FitMode::Fill);
    let out = compose(200, 100, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!(px(&out, 0, 0), RED);
    assert_eq!(px(&out, 199, 99), RED);
    assert_eq!(px(&out, 0, 99), RED);
    assert_eq!(px(&out, 199, 0), RED);
}

#[test]
fn compose_opacity_half_blends_to_midpoint() {
    // White at 0.5 over black: 127.5 rounds to 128 (tolerance 1).
    let src = Frame::solid(8, 8, 255, 255, 255, 255);
    let t = tr(FitMode::Stretch);
    let out = compose(8, 8, rgba(0, 0, 0), &[layer(&src, &t, 0.5)]);
    let v = px(&out, 4, 4);
    for ch in &v[..3] {
        assert!(
            (i32::from(*ch) - 128).abs() <= 1,
            "channel {ch} not near midpoint"
        );
    }
    assert_eq!(v[3], 255, "canvas stays opaque");
}

#[test]
fn compose_opacity_half_blends_grey_midpoint() {
    // Layer 200 over background 100 at 0.5 -> 150 (+-1).
    let src = Frame::solid(4, 4, 200, 200, 200, 255);
    let t = tr(FitMode::Stretch);
    let out = compose(4, 4, rgba(100, 100, 100), &[layer(&src, &t, 0.5)]);
    let v = px(&out, 2, 2);
    assert!((i32::from(v[0]) - 150).abs() <= 1, "got {:?}", v);
}

#[test]
fn compose_opacity_zero_leaves_canvas_unchanged() {
    let src = Frame::solid(4, 4, 255, 255, 255, 255);
    let t = tr(FitMode::Stretch);
    let bg = rgba(10, 20, 30);
    let out = compose(4, 4, bg, &[layer(&src, &t, 0.0)]);
    assert_eq!(out, Frame::solid(4, 4, 30, 20, 10, 255));
}

#[test]
fn compose_opacity_nan_treated_as_zero() {
    let src = Frame::solid(4, 4, 255, 255, 255, 255);
    let t = tr(FitMode::Stretch);
    let out = compose(4, 4, rgba(10, 20, 30), &[layer(&src, &t, f64::NAN)]);
    assert_eq!(out, Frame::solid(4, 4, 30, 20, 10, 255));
}

#[test]
fn compose_transparent_pixels_do_not_change_canvas() {
    let src = Frame::solid(4, 4, 255, 255, 255, 0);
    let t = tr(FitMode::Stretch);
    let out = compose(4, 4, rgba(10, 20, 30), &[layer(&src, &t, 1.0)]);
    assert_eq!(out, Frame::solid(4, 4, 30, 20, 10, 255));
}

#[test]
fn compose_semi_transparent_source_blends_by_alpha() {
    // White with alpha 128 over opaque black: about 128 per colour channel.
    let src = Frame::solid(1, 1, 255, 255, 255, 128);
    let t = tr(FitMode::Stretch);
    let out = compose(1, 1, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    let v = px(&out, 0, 0);
    assert!((i32::from(v[2]) - 128).abs() <= 1, "got {:?}", v);
    assert_eq!(v[3], 255);
}

#[test]
fn compose_two_layers_top_wins_when_opaque() {
    let red = Frame::solid(4, 4, 0, 0, 255, 255);
    let blue = Frame::solid(4, 4, 255, 0, 0, 255);
    let t = tr(FitMode::Stretch);
    let out = compose(
        4,
        4,
        rgba(0, 0, 0),
        &[layer(&red, &t, 1.0), layer(&blue, &t, 1.0)],
    );
    assert_eq!(px(&out, 2, 2), BLUE, "later layer is on top");
}

#[test]
fn compose_two_layers_reverse_order_changes_winner() {
    let red = Frame::solid(4, 4, 0, 0, 255, 255);
    let blue = Frame::solid(4, 4, 255, 0, 0, 255);
    let t = tr(FitMode::Stretch);
    let out = compose(
        4,
        4,
        rgba(0, 0, 0),
        &[layer(&blue, &t, 1.0), layer(&red, &t, 1.0)],
    );
    assert_eq!(px(&out, 2, 2), RED);
}

#[test]
fn compose_flip_h_mirrors_two_colour_image() {
    // 4x1 source: left half red, right half blue. flip_h gives blue left, red right.
    let mut src = Frame::solid(4, 1, 0, 0, 0, 255);
    set_px(&mut src, 0, 0, RED);
    set_px(&mut src, 1, 0, RED);
    set_px(&mut src, 2, 0, BLUE);
    set_px(&mut src, 3, 0, BLUE);
    let mut t = tr(FitMode::Stretch);
    t.flip_h = true;
    let out = compose(4, 1, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!(px(&out, 0, 0), BLUE);
    assert_eq!(px(&out, 1, 0), BLUE);
    assert_eq!(px(&out, 2, 0), RED);
    assert_eq!(px(&out, 3, 0), RED);
}

#[test]
fn compose_flip_v_mirrors_vertical_image() {
    // 1x4 source: top half red, bottom half blue. flip_v puts blue on top.
    let mut src = Frame::solid(1, 4, 0, 0, 0, 255);
    set_px(&mut src, 0, 0, RED);
    set_px(&mut src, 0, 1, RED);
    set_px(&mut src, 0, 2, BLUE);
    set_px(&mut src, 0, 3, BLUE);
    let mut t = tr(FitMode::Stretch);
    t.flip_v = true;
    let out = compose(1, 4, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!(px(&out, 0, 0), BLUE);
    assert_eq!(px(&out, 0, 1), BLUE);
    assert_eq!(px(&out, 0, 2), RED);
    assert_eq!(px(&out, 0, 3), RED);
}

#[test]
fn compose_rotate_180_reverses_both_axes() {
    let mut src = Frame::solid(4, 1, 0, 0, 0, 255);
    set_px(&mut src, 0, 0, RED);
    set_px(&mut src, 1, 0, RED);
    set_px(&mut src, 2, 0, BLUE);
    set_px(&mut src, 3, 0, BLUE);
    let mut t = tr(FitMode::Stretch);
    t.rotate_90 = 2;
    let out = compose(4, 1, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!(px(&out, 0, 0), BLUE);
    assert_eq!(px(&out, 3, 0), RED);
}

#[test]
fn compose_rotate_90_turns_left_right_split_into_top_bottom() {
    // 4x2 source: left half red, right half blue. Clockwise rotation: red on top, blue below.
    let mut src = Frame::solid(4, 2, 0, 0, 0, 255);
    for y in 0..2 {
        set_px(&mut src, 0, y, RED);
        set_px(&mut src, 1, y, RED);
        set_px(&mut src, 2, y, BLUE);
        set_px(&mut src, 3, y, BLUE);
    }
    let mut t = tr(FitMode::Fit);
    t.rotate_90 = 1;
    let out = compose(2, 4, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!(px(&out, 0, 0), RED);
    assert_eq!(px(&out, 1, 1), RED);
    assert_eq!(px(&out, 0, 2), BLUE);
    assert_eq!(px(&out, 1, 3), BLUE);
}

#[test]
fn compose_crop_removes_coloured_edge_strip() {
    // 4x1 source: green first column, red elsewhere. Crop 25 % from the left removes green.
    let mut src = Frame::solid(4, 1, 0, 0, 255, 255);
    set_px(&mut src, 0, 0, GREEN);
    let mut t = tr(FitMode::Fit);
    t.crop = Crop {
        left: 0.25,
        right: 0.0,
        top: 0.0,
        bottom: 0.0,
    };
    let out = compose(3, 1, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    for x in 0..3 {
        assert_eq!(px(&out, x, 0), RED, "column {x} must be red, no green");
    }
}

#[test]
fn compose_layer_larger_than_canvas_is_clipped() {
    // Custom scale 4 on a 100x100 source in a 10x10 canvas gives a 40x40 layer, clipped.
    let src = Frame::solid(100, 100, 0, 0, 255, 255);
    let mut t = tr(FitMode::Custom);
    t.scale = 4.0;
    let out = compose(10, 10, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!((out.width, out.height), (10, 10));
    assert_eq!(out, Frame::solid(10, 10, 0, 0, 255, 255));
}

#[test]
fn compose_mismatched_buffer_is_ignored() {
    let bad = Frame {
        width: 4,
        height: 4,
        bgra: vec![255; 10],
    };
    let t = tr(FitMode::Stretch);
    let out = compose(4, 4, rgba(10, 20, 30), &[layer(&bad, &t, 1.0)]);
    assert_eq!(out, Frame::solid(4, 4, 30, 20, 10, 255));
}

#[test]
fn compose_zero_size_canvas_does_not_panic() {
    let src = Frame::solid(4, 4, 255, 255, 255, 255);
    let t = tr(FitMode::Fit);
    let out = compose(0, 0, rgba(0, 0, 0), &[layer(&src, &t, 1.0)]);
    assert_eq!((out.width, out.height), (0, 0));
    assert!(out.bgra.is_empty());
}

#[test]
fn compose_zero_size_layer_is_ignored() {
    let empty = Frame {
        width: 0,
        height: 0,
        bgra: Vec::new(),
    };
    let t = tr(FitMode::Fit);
    let out = compose(4, 4, rgba(10, 20, 30), &[layer(&empty, &t, 1.0)]);
    assert_eq!(out, Frame::solid(4, 4, 30, 20, 10, 255));
}

#[test]
fn compose_no_layers_gives_background() {
    let out = compose(3, 2, rgba(1, 2, 3), &[]);
    assert_eq!(out, Frame::solid(3, 2, 3, 2, 1, 255));
}

// ---------------------------------------------------------------- dissolve / dip

#[test]
fn dissolve_t0_returns_a() {
    let a = Frame::solid(2, 2, 10, 20, 30, 255);
    let b = Frame::solid(2, 2, 200, 200, 200, 255);
    assert_eq!(dissolve(&a, &b, 0.0), a);
}

#[test]
fn dissolve_t1_returns_b() {
    let a = Frame::solid(2, 2, 10, 20, 30, 255);
    let b = Frame::solid(2, 2, 200, 100, 50, 255);
    assert_eq!(dissolve(&a, &b, 1.0), b);
}

#[test]
fn dissolve_t_half_is_midpoint() {
    let a = Frame::solid(2, 2, 0, 0, 0, 255);
    let b = Frame::solid(2, 2, 200, 200, 200, 255);
    assert_eq!(
        dissolve(&a, &b, 0.5),
        Frame::solid(2, 2, 100, 100, 100, 255)
    );
}

#[test]
fn dissolve_size_mismatch_returns_first() {
    let a = Frame::solid(2, 2, 10, 20, 30, 255);
    let b = Frame::solid(3, 3, 200, 200, 200, 255);
    assert_eq!(dissolve(&a, &b, 0.5), a);
}

#[test]
fn dissolve_clamps_t_outside_unit_range() {
    let a = Frame::solid(2, 2, 0, 0, 0, 255);
    let b = Frame::solid(2, 2, 200, 200, 200, 255);
    assert_eq!(dissolve(&a, &b, 2.0), b);
    assert_eq!(dissolve(&a, &b, -1.0), a);
}

#[test]
fn dip_to_color_t0_is_identity() {
    let f = Frame::solid(2, 2, 200, 100, 50, 255);
    assert_eq!(dip_to_color(&f, rgba(10, 20, 30), 0.0), f);
}

#[test]
fn dip_to_color_t1_is_solid_colour() {
    let f = Frame::solid(2, 2, 200, 100, 50, 255);
    // colour r=10, g=20, b=30 -> stored as [b, g, r, a] = [30, 20, 10, 255].
    let out = dip_to_color(&f, rgba(10, 20, 30), 1.0);
    assert_eq!(out, Frame::solid(2, 2, 30, 20, 10, 255));
}

#[test]
fn dip_to_color_half_is_average() {
    let f = Frame::solid(2, 2, 200, 100, 50, 255);
    let out = dip_to_color(&f, rgba(10, 20, 30), 0.5);
    assert_eq!(out, Frame::solid(2, 2, 115, 60, 30, 255));
}

// ---------------------------------------------------------------- blurred_cover

#[test]
fn blurred_cover_output_size_matches_request() {
    let src = Frame::solid(160, 90, 200, 200, 200, 255);
    let out = blurred_cover(&src, 90, 160);
    assert_eq!((out.width, out.height), (90, 160));
    assert_eq!(out.bgra.len(), 90 * 160 * 4);
}

#[test]
fn blurred_cover_darkens_solid_source_to_60_percent() {
    let src = Frame::solid(160, 90, 200, 200, 200, 255);
    let out = blurred_cover(&src, 90, 160);
    for p in out.bgra.as_chunks::<4>().0 {
        for &c in &p[..3] {
            assert!(
                (i32::from(c) - 120).abs() <= 2,
                "expected about 0.6 * 200 = 120, got {c}"
            );
        }
    }
}

#[test]
fn blurred_cover_checker_stays_within_darkened_range() {
    // Black/white checker: after darkening by 40 %, no channel may exceed 0.6 * 255 + 2.
    let mut src = Frame::solid(4, 4, 0, 0, 0, 255);
    for y in 0..4u32 {
        for x in 0..4u32 {
            if (x + y) % 2 == 0 {
                set_px(&mut src, x, y, [255, 255, 255, 255]);
            }
        }
    }
    let out = blurred_cover(&src, 40, 40);
    let max = out
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| p[..3].iter().copied())
        .max()
        .unwrap_or(0);
    assert!(max <= 155, "blurred cover too bright: max channel {max}");
}

#[test]
fn blurred_cover_zero_size_does_not_panic() {
    let src = Frame::solid(8, 8, 200, 200, 200, 255);
    let out = blurred_cover(&src, 0, 0);
    assert_eq!((out.width, out.height), (1, 1));
}
