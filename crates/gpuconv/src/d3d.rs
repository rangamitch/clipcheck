//! GPU BGRA -> NV12 through the D3D11 video processor (worker G2).

use std::mem::ManuallyDrop;

use windows::core::{Error as WinError, Interface};
use windows::Win32::Foundation::{BOOL, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_VIDEO_COLOR_YCbCrA, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE,
    D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255, D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

use crate::ConvError;

/// Maps a Windows error to `ConvError::Windows` tagged with the failing API step.
fn werr(context: &'static str) -> impl FnOnce(WinError) -> ConvError {
    move |e| ConvError::Windows {
        context,
        message: e.message().to_string(),
    }
}

/// Hardware D3D11 device with BGRA and video support, multithread protected.
pub fn create_device(
    adapter: Option<&IDXGIAdapter>,
) -> Result<(ID3D11Device, ID3D11DeviceContext), ConvError> {
    // An explicit adapter requires the UNKNOWN driver type.
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: out-pointers reference live Option slots; the API writes valid
    // COM references into them on success. The adapter, if any, is borrowed
    // for the duration of the call only.
    unsafe {
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device as *mut _),
            None,
            Some(&mut context as *mut _),
        )
    }
    .map_err(werr("D3D11CreateDevice"))?;
    let (device, context) = match (device, context) {
        (Some(d), Some(c)) => (d, c),
        _ => {
            return Err(ConvError::Windows {
                context: "D3D11CreateDevice",
                message: "no device returned".into(),
            })
        }
    };
    let multithread: ID3D11Multithread = device
        .cast()
        .map_err(werr("QueryInterface ID3D11Multithread"))?;
    // SAFETY: multithread is a valid ID3D11Multithread obtained from this device.
    unsafe {
        let _previous = multithread.SetMultithreadProtected(true);
    }
    Ok((device, context))
}

/// Cached enumerator and processor for one input size.
struct ProcessorCache {
    in_w: u32,
    in_h: u32,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

/// Scales and converts BGRA textures into NV12 textures of a fixed size.
pub struct GpuConverter {
    cw: u32,
    ch: u32,
    device: ID3D11Device,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    cache: Option<ProcessorCache>,
}

impl GpuConverter {
    /// Converter producing `cw x ch` NV12 on `device`.
    pub fn new(device: &ID3D11Device, cw: u32, ch: u32) -> Result<Self, ConvError> {
        if cw == 0 || ch == 0 || !cw.is_multiple_of(2) || !ch.is_multiple_of(2) {
            return Err(ConvError::BadInput(format!(
                "output {cw}x{ch} must be non-zero and even"
            )));
        }
        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(werr("QueryInterface ID3D11VideoDevice"))?;
        // SAFETY: device is a valid live device; the immediate context is returned
        // as an owned COM reference.
        let context =
            unsafe { device.GetImmediateContext() }.map_err(werr("GetImmediateContext"))?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(werr("QueryInterface ID3D11VideoContext"))?;
        Ok(Self {
            cw,
            ch,
            device: device.clone(),
            video_device,
            video_context,
            cache: None,
        })
    }

    /// A new NV12 output texture of the converter's size.
    pub fn create_nv12_texture(&self) -> Result<ID3D11Texture2D, ConvError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.cw,
            Height: self.ch,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: desc is a valid local descriptor; the out-pointer references a live
        // Option slot that the API fills with a COM reference on success.
        unsafe {
            self.device
                .CreateTexture2D(&desc as *const _, None, Some(&mut texture as *mut _))
        }
        .map_err(werr("CreateTexture2D"))?;
        texture.ok_or_else(|| ConvError::Windows {
            context: "CreateTexture2D",
            message: "no texture returned".into(),
        })
    }

    /// Blits `src` (BGRA, any size) into `dst` (NV12), letterboxed.
    pub fn convert(
        &mut self,
        src: &ID3D11Texture2D,
        dst: &ID3D11Texture2D,
    ) -> Result<(), ConvError> {
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: sd is a valid, writable descriptor the API fills in.
        unsafe { src.GetDesc(&mut sd) };
        if sd.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(ConvError::BadInput(format!(
                "source format {:?} is not B8G8R8A8_UNORM",
                sd.Format
            )));
        }
        if sd.Width == 0 || sd.Height == 0 {
            return Err(ConvError::BadInput("source has zero size".into()));
        }
        let (sw, sh) = (sd.Width, sd.Height);
        let (enumerator, processor) = self.processor_for(sw, sh)?;

        let (x, y, w, h) = fit_rect(sw, sh, self.cw, self.ch);
        let dest = RECT {
            left: x as i32,
            top: y as i32,
            right: (x + w) as i32,
            bottom: (y + h) as i32,
        };
        let source = RECT {
            left: 0,
            top: 0,
            right: sw as i32,
            bottom: sh as i32,
        };
        let target = RECT {
            left: 0,
            top: 0,
            right: self.cw as i32,
            bottom: self.ch as i32,
        };
        let input_cs = color_space(D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255);
        let output_cs = color_space(D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235);
        // Black letterbox bars in limited-range YCbCr.
        let background = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                    Y: 16.0 / 255.0,
                    Cb: 128.0 / 255.0,
                    Cr: 128.0 / 255.0,
                    A: 1.0,
                },
            },
        };

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };

        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        // SAFETY: src, the enumerator and the descriptor are live for the call; the
        // out-pointer references a live Option slot.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                src,
                &enumerator,
                &input_desc as *const _,
                Some(&mut input_view as *mut _),
            )
        }
        .map_err(werr("CreateVideoProcessorInputView"))?;
        let input_view = input_view.ok_or_else(|| ConvError::Windows {
            context: "CreateVideoProcessorInputView",
            message: "no view returned".into(),
        })?;

        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        // SAFETY: dst, the enumerator and the descriptor are live for the call; the
        // out-pointer references a live Option slot.
        unsafe {
            self.video_device.CreateVideoProcessorOutputView(
                dst,
                &enumerator,
                &output_desc as *const _,
                Some(&mut output_view as *mut _),
            )
        }
        .map_err(werr("CreateVideoProcessorOutputView"))?;
        let output_view = output_view.ok_or_else(|| ConvError::Windows {
            context: "CreateVideoProcessorOutputView",
            message: "no view returned".into(),
        })?;

        let vc = &self.video_context;
        // SAFETY: processor, the colour-space, background and rect values are live
        // locals that outlive each call; the Set* calls only copy the values.
        unsafe {
            vc.VideoProcessorSetStreamColorSpace(&processor, 0, &input_cs as *const _);
            vc.VideoProcessorSetOutputColorSpace(&processor, &output_cs as *const _);
            vc.VideoProcessorSetOutputBackgroundColor(&processor, true, &background as *const _);
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source as *const _));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dest as *const _));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target as *const _));
            vc.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        }

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            // The stream does not own its input surface; ownership is taken back below.
            pInputSurface: ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        };
        let streams = [stream];
        // SAFETY: processor, output_view and streams are live for the call; the stream
        // array holds one enabled stream whose input view is valid.
        let blt = unsafe { vc.VideoProcessorBlt(&processor, &output_view, 0, &streams) };
        // Take the input view back out of the ManuallyDrop so it is released on every path.
        let [stream] = streams;
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        blt.map_err(werr("VideoProcessorBlt"))?;
        Ok(())
    }

    /// Returns the enumerator and processor for an input of `sw x sh`, rebuilding on change.
    fn processor_for(
        &mut self,
        sw: u32,
        sh: u32,
    ) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor), ConvError> {
        if let Some(c) = &self.cache {
            if c.in_w == sw && c.in_h == sh {
                return Ok((c.enumerator.clone(), c.processor.clone()));
            }
        }
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: sw,
            InputHeight: sh,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: self.cw,
            OutputHeight: self.ch,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: content is a valid local descriptor for the duration of the call.
        let enumerator = unsafe {
            self.video_device
                .CreateVideoProcessorEnumerator(&content as *const _)
        }
        .map_err(werr("CreateVideoProcessorEnumerator"))?;
        // SAFETY: enumerator is a valid enumerator created on this video device.
        let processor = unsafe { self.video_device.CreateVideoProcessor(&enumerator, 0) }
            .map_err(werr("CreateVideoProcessor"))?;
        let out = (enumerator.clone(), processor.clone());
        self.cache = Some(ProcessorCache {
            in_w: sw,
            in_h: sh,
            enumerator,
            processor,
        });
        Ok(out)
    }
}

/// Packs a D3D11 video processor colour space: BT.709 matrix, RGB full range flag 0,
/// playback usage, and the given nominal range (bits 4-5).
fn color_space(nominal: D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE) -> D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    // Bit layout: Usage(0) RGB_Range(1) YCbCr_Matrix(2) YCbCr_xvYCC(3) Nominal_Range(4-5).
    let bits = (1u32 << 2) | ((nominal.0 as u32) << 4);
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: bits }
}

/// Letterbox placement of a `sw x sh` source inside `cw x ch`, as `(x, y, w, h)`.
/// Width and height are even; the offsets are even too.
fn fit_rect(sw: u32, sh: u32, cw: u32, ch: u32) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 {
        return (0, 0, 0, 0);
    }
    let (sw64, sh64, cw64, ch64) = (sw as u64, sh as u64, cw as u64, ch as u64);
    let (w, h) = if sw64 * ch64 <= cw64 * sh64 {
        let h = ch64;
        let w = (sw64 * ch64 + sh64 / 2) / sh64;
        (w, h)
    } else {
        let w = cw64;
        let h = (sh64 * cw64 + sw64 / 2) / sw64;
        (w, h)
    };
    let w = ((w & !1).max(2)).min(cw64 & !1) as u32;
    let h = ((h & !1).max(2)).min(ch64 & !1) as u32;
    let x = ((cw - w) / 2) & !1;
    let y = ((ch - h) / 2) & !1;
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_device_and_texture() {
        let (device, _context) = match create_device(None) {
            Ok(d) => d,
            Err(_) => {
                println!("SKIPPED: no D3D11 device");
                return;
            }
        };
        let converter = GpuConverter::new(&device, 64, 32).expect("GpuConverter::new");
        converter
            .create_nv12_texture()
            .expect("create_nv12_texture");
    }
}
