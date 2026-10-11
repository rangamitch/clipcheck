//! Black-box tests for the CPU BGRA -> NV12 path and letterbox geometry.
//! Only the public API is used.

use clipcheck_gpuconv::{bgra_to_nv12, letterbox_rect, ConvError};
use std::time::Instant;

// ---------- reference formula (BT.709 limited) ----------

fn ycalc(r: f64, g: f64, b: f64) -> i32 {
    (16.0 + 0.1826 * r + 0.6142 * g + 0.0620 * b)
        .round()
        .clamp(0.0, 255.0) as i32
}
fn ucalc(r: f64, g: f64, b: f64) -> i32 {
    (128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b)
        .round()
        .clamp(0.0, 255.0) as i32
}
fn vcalc(r: f64, g: f64, b: f64) -> i32 {
    (128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b)
        .round()
        .clamp(0.0, 255.0) as i32
}

// ---------- helpers ----------

/// Build a BGRA buffer; `f(x, y)` returns [r, g, b].
fn bgra(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Vec<u8> {
    bgra(w, h, |_, _| rgb)
}

fn y_at(out: &[u8], cw: usize, x: usize, y: usize) -> i32 {
    out[y * cw + x] as i32
}

/// (U, V) of the 2x2 chroma pair containing luma pixel (x, y).
fn uv_at(out: &[u8], cw: usize, ch: usize, x: usize, y: usize) -> (i32, i32) {
    let base = cw * ch + (y / 2) * cw + (x / 2) * 2;
    (out[base] as i32, out[base + 1] as i32)
}

fn assert_px(out: &[u8], cw: usize, ch: usize, x: usize, y: usize, exp: (i32, i32, i32), tol: i32) {
    let yy = y_at(out, cw, x, y);
    let (u, v) = uv_at(out, cw, ch, x, y);
    assert!(
        (yy - exp.0).abs() <= tol && (u - exp.1).abs() <= tol && (v - exp.2).abs() <= tol,
        "pixel ({x},{y}): got Y={yy} U={u} V={v}, expected Y={} U={} V={} (tol {tol})",
        exp.0,
        exp.1,
        exp.2
    );
}

fn assert_all(out: &[u8], cw: usize, ch: usize, exp: (i32, i32, i32), tol: i32) {
    for y in 0..ch {
        for x in 0..cw {
            assert_px(out, cw, ch, x, y, exp, tol);
        }
    }
}

fn conv(src: &[u8], sw: u32, sh: u32, cw: u32, ch: u32) -> Vec<u8> {
    let mut out = Vec::new();
    bgra_to_nv12(src, sw, sh, cw, ch, &mut out).expect("conversion should succeed");
    assert_eq!(out.len(), (cw * ch * 3 / 2) as usize, "NV12 length");
    out
}

// ---------- letterbox_rect ----------

#[test]
fn letterbox_16x9_into_16x9_fills_canvas() {
    assert_eq!(letterbox_rect(1920, 1080, 1280, 720), (0, 0, 1280, 720));
    assert_eq!(letterbox_rect(640, 360, 1280, 720), (0, 0, 1280, 720));
    assert_eq!(letterbox_rect(16, 9, 32, 18), (0, 0, 32, 18));
    assert_eq!(letterbox_rect(1280, 720, 1280, 720), (0, 0, 1280, 720));
}

#[test]
fn letterbox_4x3_into_16x9_is_pillarboxed_and_centred() {
    // h = 720, w = round(640*720/480) = 960, x = (1280-960)/2 = 160
    assert_eq!(letterbox_rect(640, 480, 1280, 720), (160, 0, 960, 720));
}

#[test]
fn letterbox_21x9_into_16x9_is_letterboxed_and_centred() {
    // w = 1280, h = round(1080*1280/2560) = 540, y = (720-540)/2 = 90
    assert_eq!(letterbox_rect(2560, 1080, 1280, 720), (0, 90, 1280, 540));
}

#[test]
fn letterbox_portrait_into_16x9_is_pillarboxed() {
    // h = 720, w = round(1080*720/1920) = 405 -> even 404, x = (1280-404)/2 = 438
    assert_eq!(letterbox_rect(1080, 1920, 1280, 720), (438, 0, 404, 720));
}

#[test]
fn letterbox_1x1_into_2x2() {
    assert_eq!(letterbox_rect(1, 1, 2, 2), (0, 0, 2, 2));
}

#[test]
fn letterbox_zero_inputs_give_zero_rect() {
    assert_eq!(letterbox_rect(0, 1080, 1280, 720), (0, 0, 0, 0));
    assert_eq!(letterbox_rect(1920, 0, 1280, 720), (0, 0, 0, 0));
    assert_eq!(letterbox_rect(1920, 1080, 0, 720), (0, 0, 0, 0));
    assert_eq!(letterbox_rect(1920, 1080, 1280, 0), (0, 0, 0, 0));
    assert_eq!(letterbox_rect(0, 0, 0, 0), (0, 0, 0, 0));
}

#[test]
fn letterbox_results_are_even_inside_canvas_and_at_least_2() {
    let sources = [1u32, 2, 3, 5, 7, 16, 100, 1000, 1921, 4096];
    let canvases = [
        (2u32, 2u32),
        (16, 2),
        (2, 16),
        (100, 64),
        (1280, 720),
        (640, 480),
        (1920, 1080),
        (1000, 8),
    ];
    let mut n = 0;
    for &(cw, ch) in &canvases {
        for &sw in &sources {
            for &sh in &sources {
                let (x, y, w, h) = letterbox_rect(sw, sh, cw, ch);
                n += 1;
                assert!(
                    w >= 2 && h >= 2,
                    "{sw}x{sh} in {cw}x{ch}: tiny rect {w}x{h}"
                );
                assert!(
                    w % 2 == 0 && h % 2 == 0,
                    "{sw}x{sh} in {cw}x{ch}: odd size {w}x{h}"
                );
                assert!(
                    x % 2 == 0 && y % 2 == 0,
                    "{sw}x{sh} in {cw}x{ch}: odd offset ({x},{y})"
                );
                assert!(
                    x + w <= cw && y + h <= ch,
                    "{sw}x{sh} in {cw}x{ch}: rect ({x},{y},{w},{h}) outside canvas"
                );
            }
        }
    }
    assert!(n > 0);
}

// ---------- bgra_to_nv12 error handling ----------

#[test]
fn bad_input_cases_return_bad_input() {
    let good = solid(2, 2, [1, 2, 3]);
    let mut out = Vec::new();

    let cases: Vec<(&str, Result<(), ConvError>)> = vec![
        ("sw=0", bgra_to_nv12(&good, 0, 2, 2, 2, &mut out)),
        ("sh=0", bgra_to_nv12(&good, 2, 0, 2, 2, &mut out)),
        ("cw=0", bgra_to_nv12(&good, 2, 2, 0, 2, &mut out)),
        ("ch=0", bgra_to_nv12(&good, 2, 2, 2, 0, &mut out)),
        ("cw odd", bgra_to_nv12(&good, 2, 2, 3, 2, &mut out)),
        ("ch odd", bgra_to_nv12(&good, 2, 2, 2, 3, &mut out)),
        (
            "src one byte short",
            bgra_to_nv12(&good[..15], 2, 2, 2, 2, &mut out),
        ),
        (
            "src four bytes long",
            bgra_to_nv12(&[good.clone(), vec![0u8; 4]].concat(), 2, 2, 2, 2, &mut out),
        ),
        ("src empty", bgra_to_nv12(&[], 2, 2, 2, 2, &mut out)),
    ];
    for (name, r) in cases {
        assert!(
            matches!(r, Err(ConvError::BadInput(_))),
            "{name}: expected BadInput"
        );
    }
}

// ---------- output layout and solid colours ----------

#[test]
fn nv12_length_is_w_h_3_over_2() {
    let out = conv(&solid(1920, 1080, [0, 0, 0]), 1920, 1080, 1280, 720);
    assert_eq!(out.len(), 1280 * 720 * 3 / 2);
}

#[test]
fn solid_white_same_size() {
    let out = conv(&solid(16, 16, [255, 255, 255]), 16, 16, 16, 16);
    assert_all(&out, 16, 16, (235, 128, 128), 0);
}

#[test]
fn solid_black_same_size() {
    let out = conv(&solid(16, 16, [0, 0, 0]), 16, 16, 16, 16);
    assert_all(&out, 16, 16, (16, 128, 128), 0);
}

#[test]
fn solid_red_green_blue_same_size() {
    let o = conv(&solid(16, 16, [255, 0, 0]), 16, 16, 16, 16);
    assert_all(&o, 16, 16, (63, 102, 240), 1);
    let o = conv(&solid(16, 16, [0, 255, 0]), 16, 16, 16, 16);
    assert_all(&o, 16, 16, (173, 42, 26), 1);
    let o = conv(&solid(16, 16, [0, 0, 255]), 16, 16, 16, 16);
    assert_all(&o, 16, 16, (32, 240, 118), 1);
}

#[test]
fn solid_colours_scaled_up_and_down() {
    let colours: [([u8; 3], (i32, i32, i32)); 3] = [
        ([255, 0, 0], (63, 102, 240)),
        ([0, 255, 0], (173, 42, 26)),
        ([0, 0, 255], (32, 240, 118)),
    ];
    for (rgb, exp) in colours {
        // 2x up: 8x8 -> 16x16
        let o = conv(&solid(8, 8, rgb), 8, 8, 16, 16);
        assert_all(&o, 16, 16, exp, 1);
        // 2x down: 16x16 -> 8x8
        let o = conv(&solid(16, 16, rgb), 16, 16, 8, 8);
        assert_all(&o, 8, 8, exp, 1);
    }
}

// ---------- letterboxing ----------

#[test]
fn pillarbox_bars_are_exact_black_level() {
    // 4x3 white into 32x18 -> rect (4,0,24,18); bars at columns 0..4 and 28..32.
    let src = solid(4, 3, [255, 255, 255]);
    let mut out = vec![0xEEu8; 7]; // garbage of wrong length
    bgra_to_nv12(&src, 4, 3, 32, 18, &mut out).unwrap();
    assert_eq!(out.len(), 32 * 18 * 3 / 2);
    assert_eq!(letterbox_rect(4, 3, 32, 18), (4, 0, 24, 18));

    // full row 0 and full column checks on the bars
    for x in 0..32 {
        let inside = (4..28).contains(&x);
        if inside {
            assert_px(&out, 32, 18, x, 0, (235, 128, 128), 0);
        } else {
            assert_px(&out, 32, 18, x, 0, (16, 128, 128), 0);
        }
    }
    for y in 0..18 {
        assert_px(&out, 32, 18, 0, y, (16, 128, 128), 0);
        assert_px(&out, 32, 18, 31, y, (16, 128, 128), 0);
    }
    // whole bar columns
    for y in 0..18 {
        for x in (0..4).chain(28..32) {
            assert_px(&out, 32, 18, x, y, (16, 128, 128), 0);
        }
    }
}

#[test]
fn letterbox_bars_are_exact_black_level() {
    // 21x9 white into 32x18 -> rect (0,2,32,14); bars rows 0..2 and 16..18.
    let src = solid(21, 9, [255, 255, 255]);
    let mut out = vec![0x77u8; 5];
    bgra_to_nv12(&src, 21, 9, 32, 18, &mut out).unwrap();
    assert_eq!(out.len(), 32 * 18 * 3 / 2);
    assert_eq!(letterbox_rect(21, 9, 32, 18), (0, 2, 32, 14));

    for &y in &[0usize, 1, 16, 17] {
        for x in 0..32 {
            assert_px(&out, 32, 18, x, y, (16, 128, 128), 0);
        }
    }
    for y in 2..16 {
        for x in 0..32 {
            assert_px(&out, 32, 18, x, y, (235, 128, 128), 0);
        }
    }
}

// ---------- pixel accuracy ----------

#[test]
fn checker_4x4_same_size_luma_exact_chroma_within_1() {
    let (w, h) = (4usize, 4usize);
    let f = |x: usize, y: usize| -> [u8; 3] {
        [
            (y * 60 + x * 15) as u8,
            (255 - x * 50 - y * 20) as u8,
            (x * 70 + y * 13) as u8,
        ]
    };
    let src = bgra(w, h, f);
    let out = conv(&src, 4, 4, 4, 4);

    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            let yy = y_at(&out, 4, x, y);
            assert_eq!(
                yy,
                ycalc(r as f64, g as f64, b as f64),
                "luma at ({x},{y}) rgb=({r},{g},{b})"
            );
        }
    }
    // chroma per 2x2 block from averaged RGB
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let mut acc = [0f64; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let [r, g, b] = f(bx * 2 + dx, by * 2 + dy);
                    acc[0] += r as f64;
                    acc[1] += g as f64;
                    acc[2] += b as f64;
                }
            }
            let (r, g, b) = (acc[0] / 4.0, acc[1] / 4.0, acc[2] / 4.0);
            let base = 4 * 4 + by * 4 + bx * 2;
            let (u, v) = (out[base] as i32, out[base + 1] as i32);
            assert!(
                (u - ucalc(r, g, b)).abs() <= 1,
                "U block ({bx},{by}): got {u}, expected {}",
                ucalc(r, g, b)
            );
            assert!(
                (v - vcalc(r, g, b)).abs() <= 1,
                "V block ({bx},{by}): got {v}, expected {}",
                vcalc(r, g, b)
            );
        }
    }
}

// ---------- overwrite semantics ----------

#[test]
fn out_is_fully_overwritten_when_garbage_and_wrong_length() {
    let src = solid(16, 16, [255, 255, 255]);
    let mut out = vec![0xABu8; 99_999];
    bgra_to_nv12(&src, 16, 16, 16, 16, &mut out).unwrap();
    assert_eq!(out.len(), 16 * 16 * 3 / 2);
    assert_all(&out, 16, 16, (235, 128, 128), 0);

    let mut short = vec![7u8; 3];
    bgra_to_nv12(&src, 16, 16, 16, 16, &mut short).unwrap();
    assert_eq!(short.len(), 16 * 16 * 3 / 2);
    assert_all(&short, 16, 16, (235, 128, 128), 0);
}

// ---------- timing ----------

#[test]
fn full_hd_to_720p_completes() {
    let src = bgra(1920, 1080, |x, y| {
        [
            (x * 255 / 1919) as u8,
            (y * 255 / 1079) as u8,
            ((x + y) % 256) as u8,
        ]
    });
    let mut out = Vec::new();
    let t = Instant::now();
    bgra_to_nv12(&src, 1920, 1080, 1280, 720, &mut out).unwrap();
    let ms = t.elapsed().as_millis();
    println!("TIMING 1920x1080 -> 1280x720 elapsed_ms={ms}");
    assert_eq!(out.len(), 1280 * 720 * 3 / 2);
}
