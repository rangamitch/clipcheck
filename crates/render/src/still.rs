//! Still-frame export (Windows only): one composed frame, written as PNG or JPEG with WIC.
//! Owner: worker `render-engine`.

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use clipcheck_timeline::export::AudioLayout;
use clipcheck_timeline::model::Project;
use clipcheck_timeline::plan::derive_plan;
use clipcheck_timeline::time::Tick;
use windows::core::{Interface, GUID, PCWSTR, PWSTR, VARIANT};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
    GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode,
    IWICImagingFactory, IWICPalette, IWICStream, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::VT_R4;

use crate::error::RenderError;
use crate::frame::Frame;
use crate::job::JobControl;
use crate::pipeline::{self, PipelineConfig, Sink};
use crate::sources::{out_err, src_err, ComGuard, MfAudioProvider, MfVideoProvider};
use crate::titles::GdiTitleProvider;

/// GENERIC_WRITE access right (winnt.h), as `IWICStream::InitializeFromFilename` takes it.
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Keeps the first frame the pipeline delivers.
struct FirstFrame {
    frame: Option<Frame>,
}

impl Sink for FirstFrame {
    fn video(&mut self, frame: &Frame, _index: u64) -> Result<(), RenderError> {
        if self.frame.is_none() {
            self.frame = Some(frame.clone());
        }
        Ok(())
    }

    fn audio(&mut self, _track: usize, _samples: &[f32]) -> Result<(), RenderError> {
        Ok(())
    }
}

/// Renders the canvas at timeline time `at` (all visible layers, titles) as one BGRA frame at
/// the canvas size. The frame is the first output of a one-frame plan starting at `at`, snapped
/// to the project frame rate.
pub fn render_frame(project: &Project, at: Tick) -> Result<Frame, RenderError> {
    render_frame_with(project, at, &JobControl::new())
}

/// Like [`render_frame`], but observes the caller's cancel and pause flags. Returns
/// `Err(RenderError::Cancelled)` when cancellation was already requested.
pub fn render_frame_with(
    project: &Project,
    at: Tick,
    ctl: &JobControl,
) -> Result<Frame, RenderError> {
    if ctl.is_cancelled() {
        return Err(RenderError::Cancelled);
    }
    let canvas = &project.canvas;
    let fps = canvas.fps;
    let start = at.snap_to_frame(fps);
    let plan = derive_plan(
        project,
        Some((start, start.saturating_add(fps.frame_duration()))),
    );
    if plan.start >= plan.end {
        return Err(RenderError::Invalid(
            "the still-frame time is outside the timeline".to_string(),
        ));
    }
    let cfg = PipelineConfig {
        out_w: canvas.width,
        out_h: canvas.height,
        fps,
        sample_rate: 48_000,
        layout: AudioLayout::Mix,
        audio_tracks: 1,
        limiter: false,
        loudness_gain_db: 0.0,
        video: true,
        audio: false,
    };
    let mut video = MfVideoProvider::new(project);
    let mut audio = MfAudioProvider::new(project);
    let mut titles = GdiTitleProvider;
    let mut sink = FirstFrame { frame: None };
    pipeline::run(
        &plan,
        &cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        ctl,
        &mut |_| {},
    )?;
    sink.frame
        .ok_or_else(|| RenderError::Source("no frame was rendered at that time".to_string()))
}

/// Encodes a frame to PNG or JPEG (WIC) and writes it to `dst` via a `.part` file. `format` is
/// "png" or "jpg"; `quality` 1..=100 applies to jpg.
pub fn write_image(
    frame: &Frame,
    dst: &Path,
    format: &str,
    quality: u32,
) -> Result<(), RenderError> {
    let jpg = match format {
        "png" => false,
        "jpg" => true,
        other => {
            return Err(RenderError::Invalid(format!(
                "still format must be png or jpg (got {other})"
            )))
        }
    };
    let expected = (frame.width as usize)
        .saturating_mul(frame.height as usize)
        .saturating_mul(4);
    if frame.width == 0 || frame.height == 0 || frame.bgra.len() != expected {
        return Err(RenderError::Invalid(
            "frame buffer does not match its size".to_string(),
        ));
    }
    if let Some(dir) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| RenderError::Output(format!("create_dir_all: {e}")))?;
    }
    let part = part_path(dst);
    let _ = std::fs::remove_file(&part);
    if let Err(e) = encode(frame, &part, jpg, quality) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, dst).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        RenderError::Output(format!("rename: {e}"))
    })
}

/// `<dst>.part`, the temporary file written before the rename.
fn part_path(dst: &Path) -> PathBuf {
    let mut s: OsString = dst.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Sets the WIC `ImageQuality` option (0.0..=1.0) on a JPEG encoder's frame options.
fn set_jpeg_quality(bag: Option<&IPropertyBag2>, quality: u32) -> Result<(), RenderError> {
    let Some(bag) = bag else {
        return Err(RenderError::Output(
            "the JPEG encoder gave no options bag".to_string(),
        ));
    };
    let mut name: Vec<u16> = "ImageQuality"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let prop = PROPBAG2 {
        dwType: 1, // PROPBAG2_TYPE_DATA
        vt: VT_R4,
        cfType: 0,
        dwHint: 0,
        pstrName: PWSTR(name.as_mut_ptr()),
        clsid: GUID::zeroed(),
    };
    let value = VARIANT::from(quality.clamp(1, 100) as f32 / 100.0);
    // SAFETY: `prop` names a NUL-terminated buffer that outlives the call; `value` is a live
    // VT_R4 variant.
    unsafe { bag.Write(1, &prop, &value) }.map_err(|e| out_err("IPropertyBag2::Write", e))
}

/// Writes `frame` to `part` with the WIC container for PNG or JPEG.
fn encode(frame: &Frame, part: &Path, jpg: bool, quality: u32) -> Result<(), RenderError> {
    let _com = ComGuard::enter();
    // SAFETY: CLSID constant, no outer unknown.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| src_err("CoCreateInstance", e))?;

    // SAFETY: frame.bgra holds width * height * 4 bytes and outlives the calls below.
    let source = unsafe {
        factory.CreateBitmapFromMemory(
            frame.width,
            frame.height,
            &GUID_WICPixelFormat32bppBGRA,
            frame.width * 4,
            &frame.bgra,
        )
    }
    .map_err(|e| out_err("CreateBitmapFromMemory", e))?;

    // JPEG has no alpha, so it is written as BGR; PNG keeps BGRA.
    let target = if jpg {
        GUID_WICPixelFormat24bppBGR
    } else {
        GUID_WICPixelFormat32bppBGRA
    };
    // SAFETY: plain factory call.
    let converter = unsafe { factory.CreateFormatConverter() }
        .map_err(|e| out_err("CreateFormatConverter", e))?;
    // SAFETY: source is live; the destination GUID is a constant; no palette.
    unsafe {
        converter.Initialize(
            &source,
            &target,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .map_err(|e| out_err("IWICFormatConverter::Initialize", e))?;

    let wide: Vec<u16> = part
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and alive for the call.
    let stream: IWICStream =
        unsafe { factory.CreateStream() }.map_err(|e| out_err("CreateStream", e))?;
    // SAFETY: the path is valid for the call; GENERIC_WRITE is a plain access mask.
    unsafe { stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE) }
        .map_err(|e| out_err("InitializeFromFilename", e))?;

    let container = if jpg {
        &GUID_ContainerFormatJpeg
    } else {
        &GUID_ContainerFormatPng
    };
    // SAFETY: container GUID constant; no vendor GUID.
    let encoder = unsafe { factory.CreateEncoder(container, std::ptr::null()) }
        .map_err(|e| out_err("CreateEncoder", e))?;
    let stream_iface: IStream = stream.cast().map_err(|e| out_err("IWICStream cast", e))?;
    // SAFETY: the stream is live and writable; NoCache keeps the encoder from buffering the file.
    unsafe { encoder.Initialize(&stream_iface, WICBitmapEncoderNoCache) }
        .map_err(|e| out_err("IWICBitmapEncoder::Initialize", e))?;

    let mut options: Option<IPropertyBag2> = None;
    let mut frame_enc: Option<IWICBitmapFrameEncode> = None;
    // SAFETY: out-pointers reference live Option slots.
    unsafe { encoder.CreateNewFrame(&mut frame_enc, &mut options) }
        .map_err(|e| out_err("CreateNewFrame", e))?;
    let frame_enc = frame_enc
        .ok_or_else(|| RenderError::Output("CreateNewFrame returned no frame".to_string()))?;
    if jpg {
        set_jpeg_quality(options.as_ref(), quality)?;
    }
    // SAFETY: the options bag (if any) is live for the call.
    unsafe { frame_enc.Initialize(options.as_ref()) }
        .map_err(|e| out_err("IWICBitmapFrameEncode::Initialize", e))?;
    // SAFETY: plain size on a live frame.
    unsafe { frame_enc.SetSize(frame.width, frame.height) }.map_err(|e| out_err("SetSize", e))?;
    let mut pixel_format = target;
    // SAFETY: pixel_format is a valid in/out GUID.
    unsafe { frame_enc.SetPixelFormat(&mut pixel_format) }
        .map_err(|e| out_err("SetPixelFormat", e))?;
    // SAFETY: the converter is a live bitmap source; a null rectangle means the whole image.
    unsafe { frame_enc.WriteSource(&converter, std::ptr::null()) }
        .map_err(|e| out_err("WriteSource", e))?;
    // SAFETY: commits the frame, then the encoder, in that order.
    unsafe { frame_enc.Commit() }.map_err(|e| out_err("Commit", e))?;
    // SAFETY: as above.
    unsafe { encoder.Commit() }.map_err(|e| out_err("Commit", e))?;
    Ok(())
}
