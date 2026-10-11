//! Black-box tests for the D3D11 video-processor BGRA -> NV12 path.
//! Only the public API is used. Skips cleanly when no hardware D3D11 device exists.
#![cfg(windows)]

use clipcheck_gpuconv::d3d::{create_device, GpuConverter};
use clipcheck_gpuconv::{bgra_to_nv12, letterbox_rect, ConvError};
use std::time::Instant;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};

// ---------- setup helpers ----------

fn gpu() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    match create_device(None) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("SKIPPED: no D3D11 device: {e}");
            None
        }
    }
}

/// Tightly packed BGRA buffer, `f(x, y) -> (r, g, b)`.
fn bgra_fill(w: u32, h: u32, f: impl Fn(u32, u32) -> (u8, u8, u8)) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = f(x, y);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    bgra_fill(w, h, |_, _| (r, g, b))
}

fn bgra_texture(dev: &ID3D11Device, w: u32, h: u32, pixels: &[u8]) -> ID3D11Texture2D {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const _,
        SysMemPitch: w * 4,
        SysMemSlicePitch: 0,
    };
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }
        .expect("CreateTexture2D (BGRA source)");
    tex.expect("BGRA source texture")
}

/// A BGRA-sized texture of another format, used for the wrong-format error test.
fn rgba_texture_no_data(dev: &ID3D11Device, w: u32, h: u32) -> ID3D11Texture2D {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, None, Some(&mut tex)) }
        .expect("CreateTexture2D (RGBA source)");
    tex.expect("RGBA texture")
}

/// Copies `dst` (NV12) to a staging texture and returns tightly packed (Y plane, UV plane).
fn read_nv12(
    ctx: &ID3D11DeviceContext,
    dev: &ID3D11Device,
    dst: &ID3D11Texture2D,
) -> (Vec<u8>, Vec<u8>) {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    unsafe { dst.GetDesc(&mut d) };
    let (cw, ch) = (d.Width as usize, d.Height as usize);
    d.Usage = D3D11_USAGE_STAGING;
    d.BindFlags = 0;
    d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    d.MiscFlags = 0;
    let mut staging = None;
    unsafe { dev.CreateTexture2D(&d, None, Some(&mut staging)) }
        .expect("CreateTexture2D (staging)");
    let staging = staging.expect("staging texture");
    unsafe {
        ctx.CopyResource(&staging, dst);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
            .expect("Map staging");
        let base = m.pData as *const u8;
        let pitch = m.RowPitch as usize;
        let mut y_plane = Vec::with_capacity(cw * ch);
        for row in 0..ch {
            y_plane.extend_from_slice(std::slice::from_raw_parts(base.add(row * pitch), cw));
        }
        let mut uv_plane = Vec::with_capacity(cw * ch / 2);
        let uv_base = base.add(pitch * ch);
        for row in 0..ch / 2 {
            uv_plane.extend_from_slice(std::slice::from_raw_parts(uv_base.add(row * pitch), cw));
        }
        ctx.Unmap(&staging, 0);
        (y_plane, uv_plane)
    }
}

// ---------- measurement helpers ----------

fn mean(v: impl Iterator<Item = u8>) -> f64 {
    let (mut s, mut n) = (0f64, 0usize);
    for x in v {
        s += x as f64;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        s / n as f64
    }
}

/// Mean of a rectangle (x, y, w, h) of the Y plane (plane width `cw`).
fn y_rect_mean(y: &[u8], cw: usize, (x0, y0, w, h): (u32, u32, u32, u32)) -> f64 {
    let (x0, y0, w, h) = (x0 as usize, y0 as usize, w as usize, h as usize);
    mean((y0..y0 + h).flat_map(|r| y[r * cw + x0..r * cw + x0 + w].iter().copied()))
}

/// Mean U and V over a rectangle given in luma coordinates (chroma sampled at (x/2, y/2)).
fn uv_rect_mean(uv: &[u8], cw: usize, (x0, y0, w, h): (u32, u32, u32, u32)) -> (f64, f64) {
    let (x0, y0, w, h) = (
        x0 as usize / 2,
        y0 as usize / 2,
        w as usize / 2,
        h as usize / 2,
    );
    let row = |r: usize| (x0..x0 + w).map(move |c| r * cw + c * 2);
    let idx: Vec<usize> = (y0..y0 + h).flat_map(row).collect();
    let u = mean(idx.iter().map(|&i| uv[i]));
    let v = mean(idx.iter().map(|&i| uv[i + 1]));
    (u, v)
}

fn u_mean(uv: &[u8]) -> f64 {
    mean(uv.iter().step_by(2).copied())
}
fn v_mean(uv: &[u8]) -> f64 {
    mean(uv.iter().skip(1).step_by(2).copied())
}

fn cpu_reference(src: &[u8], sw: u32, sh: u32, cw: u32, ch: u32) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    bgra_to_nv12(src, sw, sh, cw, ch, &mut out).expect("CPU reference conversion");
    let ysz = (cw * ch) as usize;
    (out[..ysz].to_vec(), out[ysz..].to_vec())
}

fn full(w: u32, h: u32) -> (u32, u32, u32, u32) {
    (0, 0, w, h)
}

// ---------- tests ----------

#[test]
fn solid_white_black_and_red_flat_colours() {
    let Some((dev, ctx)) = gpu() else { return };
    let mut conv = GpuConverter::new(&dev, 320, 180).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");

    // White
    let src = bgra_texture(&dev, 320, 180, &solid(320, 180, 255, 255, 255));
    conv.convert(&src, &dst).expect("convert white");
    let (y, uv) = read_nv12(&ctx, &dev, &dst);
    let (ym, um, vm) = (mean(y.iter().copied()), u_mean(&uv), v_mean(&uv));
    println!("white: Y={ym:.2} U={um:.2} V={vm:.2}");
    assert!((232.0..=238.0).contains(&ym), "white Y mean {ym}");
    assert!((125.0..=131.0).contains(&um), "white U mean {um}");
    assert!((125.0..=131.0).contains(&vm), "white V mean {vm}");

    // Black
    let src = bgra_texture(&dev, 320, 180, &solid(320, 180, 0, 0, 0));
    conv.convert(&src, &dst).expect("convert black");
    let (y, uv) = read_nv12(&ctx, &dev, &dst);
    let (ym, um, vm) = (mean(y.iter().copied()), u_mean(&uv), v_mean(&uv));
    println!("black: Y={ym:.2} U={um:.2} V={vm:.2}");
    assert!((14.0..=19.0).contains(&ym), "black Y mean {ym}");
    assert!((125.0..=131.0).contains(&um), "black U mean {um}");
    assert!((125.0..=131.0).contains(&vm), "black V mean {vm}");

    // Pure red vs CPU reference
    let red = solid(320, 180, 255, 0, 0);
    let src = bgra_texture(&dev, 320, 180, &red);
    conv.convert(&src, &dst).expect("convert red");
    let (gy, guv) = read_nv12(&ctx, &dev, &dst);
    let (ry, ruv) = cpu_reference(&red, 320, 180, 320, 180);
    let (gym, gum, gvm) = (mean(gy.iter().copied()), u_mean(&guv), v_mean(&guv));
    let (rym, rum, rvm) = (mean(ry.iter().copied()), u_mean(&ruv), v_mean(&ruv));
    println!("red GPU: Y={gym:.2} U={gum:.2} V={gvm:.2}");
    println!("red CPU: Y={rym:.2} U={rum:.2} V={rvm:.2}");
    assert!((gym - rym).abs() <= 4.0, "red Y gpu {gym} vs cpu {rym}");
    assert!((gum - rum).abs() <= 4.0, "red U gpu {gum} vs cpu {rum}");
    assert!((gvm - rvm).abs() <= 4.0, "red V gpu {gvm} vs cpu {rvm}");
}

#[test]
fn letterbox_4_3_white_in_16_9() {
    let Some((dev, ctx)) = gpu() else { return };
    let mut conv = GpuConverter::new(&dev, 320, 180).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");
    let src = bgra_texture(&dev, 240, 180, &solid(240, 180, 255, 255, 255));
    conv.convert(&src, &dst).expect("convert 4:3 white");
    let (y, uv) = read_nv12(&ctx, &dev, &dst);

    let rect = letterbox_rect(240, 180, 320, 180);
    println!("letterbox_rect(240,180,320,180) = {rect:?}");
    let (lx, ly, lw, lh) = rect;
    assert!(
        lx >= 10 && lx + lw <= 310,
        "rect should leave >=10px bars: {rect:?}"
    );

    let inside = y_rect_mean(&y, 320, (lx, ly, lw, lh));
    println!("inside letterbox Y mean = {inside:.2}");
    assert!((225.0..=240.0).contains(&inside), "inside Y mean {inside}");

    let left = y_rect_mean(&y, 320, (0, 0, 10, 180));
    let right = y_rect_mean(&y, 320, (310, 0, 10, 180));
    println!("bar Y means: left={left:.2} right={right:.2}");
    for (name, m) in [("left", left), ("right", right)] {
        assert!((14.0..=19.0).contains(&m), "{name} bar Y mean {m}");
    }
    let (ul, vl) = uv_rect_mean(&uv, 320, (0, 0, 10, 180));
    let (ur, vr) = uv_rect_mean(&uv, 320, (310, 0, 10, 180));
    println!("bar UV means: left U={ul:.2} V={vl:.2}; right U={ur:.2} V={vr:.2}");
    for m in [ul, vl, ur, vr] {
        assert!((125.0..=131.0).contains(&m), "bar chroma mean {m}");
    }
}

#[test]
fn scaling_gradient_matches_cpu_reference_per_block() {
    let Some((dev, ctx)) = gpu() else { return };
    let (sw, sh, cw, ch) = (1280u32, 720u32, 640u32, 360u32);
    let grad = bgra_fill(sw, sh, |x, y| {
        ((x * 255 / sw) as u8, (y * 255 / sh) as u8, 128)
    });
    let mut conv = GpuConverter::new(&dev, cw, ch).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");
    let src = bgra_texture(&dev, sw, sh, &grad);
    conv.convert(&src, &dst).expect("convert gradient");
    let (gy, _) = read_nv12(&ctx, &dev, &dst);
    let (ry, _) = cpu_reference(&grad, sw, sh, cw, ch);

    let mut worst = 0f64;
    let mut worst_at = (0, 0);
    for by in 0..(ch / 8) {
        for bx in 0..(cw / 8) {
            let r = (by * 8..by * 8 + 8)
                .flat_map(|y| (bx * 8..bx * 8 + 8).map(move |x| (y * cw + x) as usize));
            let idx: Vec<usize> = r.collect();
            let gm = mean(idx.iter().map(|&i| gy[i]));
            let rm = mean(idx.iter().map(|&i| ry[i]));
            let d = (gm - rm).abs();
            if d > worst {
                worst = d;
                worst_at = (bx, by);
            }
        }
    }
    println!(
        "gradient 1280x720->640x360: worst 8x8 block Y diff = {worst:.2} at block {worst_at:?}"
    );
    assert!(
        worst <= 6.0,
        "block diff {worst:.2} at {worst_at:?} exceeds 6"
    );
}

#[test]
fn input_size_changes_between_calls() {
    let Some((dev, ctx)) = gpu() else { return };
    let mut conv = GpuConverter::new(&dev, 640, 360).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");
    type Step<'a> = (u32, u32, (u8, u8, u8), &'a str);
    let steps: [Step; 3] = [
        (640, 360, (255, 255, 255), "white"),
        (320, 240, (0, 0, 0), "black"),
        (640, 360, (255, 255, 255), "white"),
    ];
    for (sw, sh, (r, g, b), what) in steps {
        let src = bgra_texture(&dev, sw, sh, &solid(sw, sh, r, g, b));
        conv.convert(&src, &dst)
            .unwrap_or_else(|e| panic!("convert {sw}x{sh} {what}: {e}"));
        let (y, uv) = read_nv12(&ctx, &dev, &dst);
        let rect = letterbox_rect(sw, sh, 640, 360);
        let ym = y_rect_mean(&y, 640, rect);
        let (um, vm) = uv_rect_mean(&uv, 640, rect);
        println!("{sw}x{sh} {what}: rect={rect:?} Y={ym:.2} U={um:.2} V={vm:.2}");
        if what == "white" {
            assert!((232.0..=238.0).contains(&ym), "{sw}x{sh} white Y {ym}");
        } else {
            assert!((14.0..=19.0).contains(&ym), "{sw}x{sh} black Y {ym}");
        }
        assert!(
            (125.0..=131.0).contains(&um) && (125.0..=131.0).contains(&vm),
            "{sw}x{sh} {what} chroma {um},{vm}"
        );
        let _ = full(640, 360);
    }
}

#[test]
fn errors_bad_input() {
    let Some((dev, _ctx)) = gpu() else { return };
    assert!(
        matches!(GpuConverter::new(&dev, 0, 180), Err(ConvError::BadInput(_))),
        "width 0"
    );
    assert!(
        matches!(
            GpuConverter::new(&dev, 321, 180),
            Err(ConvError::BadInput(_))
        ),
        "odd width"
    );
    let mut conv = GpuConverter::new(&dev, 320, 180).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");
    let rgba = rgba_texture_no_data(&dev, 320, 180);
    let r = conv.convert(&rgba, &dst);
    println!("RGBA source result: {r:?}");
    assert!(matches!(r, Err(ConvError::BadInput(_))), "non-BGRA source");
}

#[test]
fn perf_2560x1440_to_1920x1080_x200() {
    let Some((dev, ctx)) = gpu() else { return };
    let src_px = bgra_fill(2560, 1440, |x, y| {
        ((x * 255 / 2560) as u8, (y * 255 / 1440) as u8, 128)
    });
    let src = bgra_texture(&dev, 2560, 1440, &src_px);
    let mut conv = GpuConverter::new(&dev, 1920, 1080).expect("new converter");
    let dst = conv.create_nv12_texture().expect("dst texture");
    let start = Instant::now();
    for _ in 0..200 {
        conv.convert(&src, &dst).expect("convert");
    }
    unsafe { ctx.Flush() };
    let _ = read_nv12(&ctx, &dev, &dst);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "PERF: 200 x 2560x1440->1920x1080 + flush + readback = {ms:.1} ms ({:.2} ms/convert)",
        ms / 200.0
    );
}
