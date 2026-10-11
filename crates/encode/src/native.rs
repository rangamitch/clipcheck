//! In-process encoder (D57): GPU BGRA -> NV12 (`gpuconv`), Media Foundation video (`hwenc`),
//! AAC (`aacenc`), fragmented MP4 segments + `segments.csv` (`mp4`). Produces the same segment
//! files the tracker, buffer and sessions expect (D58), so they are unchanged. Contracts: docs/impl/native-encoder.md, wave 3.

use std::fmt::Display;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

use clipcheck_aacenc::AacEncoder;
use clipcheck_gpuconv::d3d::GpuConverter;
use clipcheck_hwenc::{
    Codec, EncodedPacket, EncoderDescriptor, InputFrame, RateControl, Vendor, VideoEncoder,
    VideoEncoderConfig,
};
use clipcheck_mp4::fmp4::AudioTrackSpec as Mp4AudioSpec;
use clipcheck_mp4::segment::{AudioPacket, SegmentConfig, SegmentMuxer, VideoPacket};
use clipcheck_mp4::VideoCodec;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
};

use crate::pipe::{AudioTrackSpec, EncodeReport};

/// Number of NV12 textures the converter cycles through.
const POOL_SIZE: usize = 8;
/// Sample rate of all audio tracks (Hz).
const AUDIO_RATE: u32 = 48_000;
/// One 2x2 black BGRA frame (B, G, R, A per pixel).
const BLACK_2X2: [u8; 16] = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];

/// What the native backend should encode with, derived from an encoder id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeTarget {
    /// Output codec.
    pub codec: Codec,
    /// Preferred hardware vendor; `None` = software (Microsoft H.264 MFT).
    pub vendor: Option<Vendor>,
}

/// Maps an encoder id to a native target: `h264_nvenc` -> (H264, Nvidia),
/// `hevc_nvenc` -> (Hevc, Nvidia), `*_amf` -> Amd, `*_qsv` -> Intel, `libx264` -> (H264, None).
/// `None` for anything the native path cannot produce (AV1, libx265, unknown names).
pub fn native_target(encoder_name: &str) -> Option<NativeTarget> {
    let (codec, vendor) = match encoder_name {
        "h264_nvenc" => (Codec::H264, Some(Vendor::Nvidia)),
        "hevc_nvenc" => (Codec::Hevc, Some(Vendor::Nvidia)),
        "h264_amf" => (Codec::H264, Some(Vendor::Amd)),
        "hevc_amf" => (Codec::Hevc, Some(Vendor::Amd)),
        "h264_qsv" => (Codec::H264, Some(Vendor::Intel)),
        "hevc_qsv" => (Codec::Hevc, Some(Vendor::Intel)),
        "libx264" => (Codec::H264, None),
        _ => return None,
    };
    Some(NativeTarget { codec, vendor })
}

/// Settings for one native encoder session.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeParams {
    /// Directory receiving `seg-NNNNNN.mp4` and `segments.csv`. Created if missing.
    pub out_dir: PathBuf,
    /// Canvas width (even, > 0).
    pub width: u32,
    /// Canvas height (even, > 0).
    pub height: u32,
    /// Output frame rate (> 0).
    pub fps: u32,
    /// Codec and vendor.
    pub target: NativeTarget,
    /// Rate control.
    pub rate: RateControl,
    /// 0 = fastest .. 100 = best quality.
    pub quality_vs_speed: u32,
    /// Segment length = keyframe interval, seconds (> 0).
    pub segment_secs: f32,
    /// `true` (live capture): video frames are stamped from the wall clock since `start`, snapped
    /// to the fps grid. `false` (offline feeds, tests): frame n is at n / fps.
    pub wall_clock: bool,
}

/// A running in-process encoder session.
pub struct NativeEncoder {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    converter: GpuConverter,
    encoder: Box<dyn VideoEncoder>,
    hardware: bool,
    muxer: SegmentMuxer,
    audio: Vec<AacEncoder>,
    written_frames: Vec<i64>,
    /// NV12 output textures, used round-robin.
    pool: Vec<ID3D11Texture2D>,
    next_slot: usize,
    /// Hardware: BGRA upload texture for system-memory frames, with its size.
    upload: Option<(u32, u32, ID3D11Texture2D)>,
    /// Hardware: cached black NV12 texture.
    black_tex: Option<ID3D11Texture2D>,
    /// Software: cached black NV12 bytes.
    black_nv12: Vec<u8>,
    /// Software: NV12 staging texture for read-back.
    staging: Option<ID3D11Texture2D>,
    /// Software: scratch NV12 buffer for system-memory frames.
    nv12_cpu: Vec<u8>,
    canvas_w: u32,
    canvas_h: u32,
    fps: u32,
    /// Segment length in 100 ns ticks.
    segment_ticks: i64,
    wall_clock: bool,
    start: Instant,
    last_k: i64,
    next_key_pts: i64,
    frames_in: u64,
    frames_dropped: u64,
}

// SAFETY: the COM objects held here (D3D11 device, context, textures, MF transforms) are
// free-threaded; the device is created multithread-protected. A session is driven from one
// thread at a time (every method takes &mut self or &self on a single owner), so moving the
// whole session to another thread does not create concurrent access.
unsafe impl Send for NativeEncoder {}

impl NativeEncoder {
    /// Opens the encoder. `device`: the D3D11 device that owns the textures passed to
    /// [`NativeEncoder::push_texture`]; `None` creates one (`gpuconv::d3d::create_device`).
    pub fn start(
        params: NativeParams,
        device: Option<ID3D11Device>,
        audio: &[AudioTrackSpec],
    ) -> io::Result<Self> {
        let NativeParams {
            out_dir,
            width,
            height,
            fps,
            target,
            rate,
            quality_vs_speed,
            segment_secs,
            wall_clock,
        } = params;

        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(bad_input(format!(
                "canvas {width}x{height} must be non-zero and even"
            )));
        }
        if fps == 0 {
            return Err(bad_input("fps must be non-zero"));
        }
        if !segment_secs.is_finite() || segment_secs <= 0.0 {
            return Err(bad_input(format!(
                "segment_secs {segment_secs} must be finite and > 0"
            )));
        }
        std::fs::create_dir_all(&out_dir)?;

        clipcheck_hwenc::mf_startup().map_err(ctx_err("MFStartup"))?;
        let (device, context) = match device {
            Some(device) => {
                // SAFETY: device is a live D3D11 device; the immediate context is returned as an
                // owned COM reference.
                let context = unsafe { device.GetImmediateContext() }
                    .map_err(ctx_err("GetImmediateContext"))?;
                (device, context)
            }
            None => clipcheck_gpuconv::d3d::create_device(None)
                .map_err(ctx_err("create D3D11 device"))?,
        };

        let codec = target.codec;
        let gop_frames = (fps as f32 * segment_secs).round().max(1.0) as u32;
        let cfg = VideoEncoderConfig {
            codec,
            width,
            height,
            fps,
            rate,
            gop_frames,
            quality_vs_speed,
            low_latency: false,
        };

        let list = clipcheck_hwenc::enumerate(codec).map_err(ctx_err("enumerate encoders"))?;
        let mut last_err = String::from("no candidate encoders");
        let mut opened: Option<(Box<dyn VideoEncoder>, bool)> = None;
        for desc in ordered_candidates(&list, target.vendor) {
            let hw_device = if desc.hardware { Some(&device) } else { None };
            match clipcheck_hwenc::open(desc, hw_device, &cfg) {
                Ok(encoder) => {
                    opened = Some((encoder, desc.hardware));
                    break;
                }
                Err(e) => last_err = format!("{}: {e}", desc.name),
            }
        }
        let (encoder, hardware) = opened.ok_or_else(|| {
            io::Error::other(format!(
                "no Media Foundation encoder for {codec:?} / {:?}: {last_err}",
                target.vendor
            ))
        })?;

        let mut converter =
            GpuConverter::new(&device, width, height).map_err(ctx_err("create converter"))?;
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            pool.push(
                converter
                    .create_nv12_texture()
                    .map_err(ctx_err("create NV12 pool texture"))?,
            );
        }

        let (black_tex, black_nv12, staging) = if hardware {
            let black_bgra = create_bgra(&device, 2, 2, Some((&BLACK_2X2, 8)))?;
            let black = converter
                .create_nv12_texture()
                .map_err(ctx_err("create black NV12 texture"))?;
            converter
                .convert(&black_bgra, &black)
                .map_err(ctx_err("convert black frame"))?;
            (Some(black), Vec::new(), None)
        } else {
            let mut black = Vec::new();
            clipcheck_gpuconv::bgra_to_nv12(&BLACK_2X2, 2, 2, width, height, &mut black)
                .map_err(ctx_err("build black NV12 frame"))?;
            (
                None,
                black,
                Some(create_staging_nv12(&device, width, height)?),
            )
        };

        let mut aac = Vec::with_capacity(audio.len());
        let mut mp4_audio = Vec::with_capacity(audio.len());
        for spec in audio {
            let enc = AacEncoder::new(AUDIO_RATE, 2, spec.bitrate_kbps)
                .map_err(ctx_err("create AAC encoder"))?;
            mp4_audio.push(Mp4AudioSpec {
                sample_rate: AUDIO_RATE,
                channels: 2,
                asc: enc.asc().to_vec(),
                bitrate: spec.bitrate_kbps * 1000,
                title: spec.title.clone(),
            });
            aac.push(enc);
        }

        let video_codec = match codec {
            Codec::H264 => VideoCodec::H264,
            Codec::Hevc => VideoCodec::Hevc,
        };
        let muxer = SegmentMuxer::new(SegmentConfig {
            out_dir,
            segment_secs: f64::from(segment_secs),
            fps,
            codec: video_codec,
            width,
            height,
            audio: mp4_audio,
        })
        .map_err(ctx_err("create segment muxer"))?;

        let segment_ticks = (f64::from(segment_secs) * 1e7).round().max(1.0) as i64;
        Ok(Self {
            device,
            context,
            converter,
            encoder,
            hardware,
            muxer,
            written_frames: vec![0; aac.len()],
            audio: aac,
            pool,
            next_slot: 0,
            upload: None,
            black_tex,
            black_nv12,
            staging,
            nv12_cpu: Vec::new(),
            canvas_w: width,
            canvas_h: height,
            fps,
            segment_ticks,
            wall_clock,
            start: Instant::now(),
            last_k: -1,
            next_key_pts: 0,
            frames_in: 0,
            frames_dropped: 0,
        })
    }

    /// The D3D11 device the encoder works on (hand it to the capture).
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Name of the Media Foundation encoder in use.
    pub fn encoder_name(&self) -> &str {
        self.encoder.descriptor().name.as_str()
    }

    /// Encodes one BGRA texture (any size, on [`NativeEncoder::device`]), letterboxed onto the canvas.
    pub fn push_texture(&mut self, bgra: &ID3D11Texture2D) -> io::Result<()> {
        let Some((pts, force)) = self.next_timing() else {
            return Ok(());
        };
        self.encode_bgra_texture(bgra, pts, force)
    }

    /// Encodes one tightly packed BGRA frame from system memory.
    pub fn push_bgra(&mut self, bgra: &[u8], width: u32, height: u32) -> io::Result<()> {
        let expected = width as usize * height as usize * 4;
        if width == 0 || height == 0 || bgra.len() != expected {
            return Err(bad_input(format!(
                "BGRA frame {width}x{height} needs {expected} bytes, got {}",
                bgra.len()
            )));
        }
        let Some((pts, force)) = self.next_timing() else {
            return Ok(());
        };
        if self.hardware {
            let upload = self.upload_texture(bgra, width, height)?;
            self.encode_bgra_texture(&upload, pts, force)
        } else {
            clipcheck_gpuconv::bgra_to_nv12(
                bgra,
                width,
                height,
                self.canvas_w,
                self.canvas_h,
                &mut self.nv12_cpu,
            )
            .map_err(ctx_err("convert BGRA to NV12"))?;
            encode_and_mux(
                self.encoder.as_mut(),
                &mut self.muxer,
                InputFrame::Nv12 {
                    data: &self.nv12_cpu,
                    width: self.canvas_w,
                    height: self.canvas_h,
                },
                pts,
                force,
            )?;
            self.frames_in += 1;
            Ok(())
        }
    }

    /// Encodes one black frame.
    pub fn push_black(&mut self) -> io::Result<()> {
        let Some((pts, force)) = self.next_timing() else {
            return Ok(());
        };
        if self.hardware {
            let black = self
                .black_tex
                .clone()
                .ok_or_else(|| io::Error::other("black NV12 texture missing"))?;
            encode_and_mux(
                self.encoder.as_mut(),
                &mut self.muxer,
                InputFrame::Texture(&black),
                pts,
                force,
            )?;
        } else {
            encode_and_mux(
                self.encoder.as_mut(),
                &mut self.muxer,
                InputFrame::Nv12 {
                    data: self.black_nv12.as_slice(),
                    width: self.canvas_w,
                    height: self.canvas_h,
                },
                pts,
                force,
            )?;
        }
        self.frames_in += 1;
        Ok(())
    }

    /// Queues interleaved 48 kHz stereo f32 samples for audio track `track`.
    pub fn push_audio(&mut self, track: usize, samples: &[f32]) -> io::Result<()> {
        if track >= self.audio.len() {
            return Err(bad_input(format!(
                "audio track {track} out of range (have {})",
                self.audio.len()
            )));
        }
        if !samples.len().is_multiple_of(2) {
            return Err(bad_input(format!(
                "audio sample count {} is not stereo-aligned",
                samples.len()
            )));
        }
        let pts = self.written_frames[track] * 10_000_000 / i64::from(AUDIO_RATE);
        self.written_frames[track] += (samples.len() / 2) as i64;
        let packets = self.audio[track]
            .push(samples, pts)
            .map_err(ctx_err("AAC encode"))?;
        for p in packets {
            self.muxer
                .push_audio(
                    track,
                    AudioPacket {
                        data: p.data,
                        pts: p.pts,
                        samples: p.samples,
                    },
                )
                .map_err(mp4_err)?;
        }
        Ok(())
    }

    /// Video frames dropped (same frame slot twice, encoder queue full).
    pub fn frames_dropped(&self) -> u64 {
        self.frames_dropped
    }

    /// Drains the encoders, closes the last segment, and reports the segments.
    pub fn finish(self) -> io::Result<EncodeReport> {
        let NativeEncoder {
            mut encoder,
            mut muxer,
            mut audio,
            frames_in,
            frames_dropped,
            ..
        } = self;
        let name = encoder.descriptor().name.clone();

        let packets = encoder.flush().map_err(ctx_err("flush video encoder"))?;
        mux_video(&mut muxer, packets)?;
        for (track, enc) in audio.iter_mut().enumerate() {
            let packets = enc.flush().map_err(ctx_err("flush AAC encoder"))?;
            for p in packets {
                muxer
                    .push_audio(
                        track,
                        AudioPacket {
                            data: p.data,
                            pts: p.pts,
                            samples: p.samples,
                        },
                    )
                    .map_err(mp4_err)?;
            }
        }
        let segments = muxer.finish().map_err(mp4_err)?;
        Ok(EncodeReport {
            frames_in,
            segments,
            exit_ok: true,
            stderr_tail: format!("native encoder {name}; dropped {frames_dropped}"),
        })
    }

    /// Decides the timestamp and keyframe flag of the next video frame. `None` = drop it
    /// (the producer is running ahead of the wall clock).
    fn next_timing(&mut self) -> Option<(i64, bool)> {
        let k = if self.wall_clock {
            let ideal = self.start.elapsed().as_secs_f64() * f64::from(self.fps);
            let k_round = ideal.round() as i64;
            let k = k_round.max(self.last_k + 1);
            if k > k_round + 1 {
                self.frames_dropped += 1;
                return None;
            }
            k
        } else {
            self.frames_in as i64
        };
        self.last_k = k;
        let pts = k * 10_000_000 / i64::from(self.fps);
        let force = pts >= self.next_key_pts;
        if force {
            // The grid starts at the first frame, not at `start`: with the wall clock the first
            // frame can arrive well after `start`, and an absolute grid would put the second
            // keyframe less than one segment after the first, so the muxer would not cut there.
            self.next_key_pts = pts + self.segment_ticks;
        }
        Some((pts, force))
    }

    /// Uploads a tightly packed BGRA frame to the hardware upload texture and returns it.
    fn upload_texture(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> io::Result<ID3D11Texture2D> {
        let reuse = matches!(&self.upload, Some((w, h, _)) if *w == width && *h == height);
        if !reuse {
            let tex = create_bgra(&self.device, width, height, None)?;
            self.upload = Some((width, height, tex));
        }
        let (_, _, tex) = self
            .upload
            .as_ref()
            .ok_or_else(|| io::Error::other("upload texture missing"))?;
        let tex = tex.clone();
        // SAFETY: tex is a live DEFAULT BGRA texture of width x height; bgra holds exactly
        // width * height * 4 bytes (checked by the caller), so the row pitch width * 4 is in range.
        unsafe {
            self.context
                .UpdateSubresource(&tex, 0, None, bgra.as_ptr().cast(), width * 4, 0);
        }
        Ok(tex)
    }

    /// Converts `bgra` into the next pool slot and encodes it. Hardware encoders take the NV12
    /// texture directly; the software encoder reads it back into system memory first.
    fn encode_bgra_texture(
        &mut self,
        bgra: &ID3D11Texture2D,
        pts: i64,
        force: bool,
    ) -> io::Result<()> {
        let slot_index = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.pool.len();
        let slot = self.pool[slot_index].clone();
        self.converter
            .convert(bgra, &slot)
            .map_err(ctx_err("convert BGRA to NV12"))?;
        if self.hardware {
            encode_and_mux(
                self.encoder.as_mut(),
                &mut self.muxer,
                InputFrame::Texture(&slot),
                pts,
                force,
            )?;
        } else {
            let data = self.read_nv12(&slot)?;
            encode_and_mux(
                self.encoder.as_mut(),
                &mut self.muxer,
                InputFrame::Nv12 {
                    data: &data,
                    width: self.canvas_w,
                    height: self.canvas_h,
                },
                pts,
                force,
            )?;
        }
        self.frames_in += 1;
        Ok(())
    }

    /// Copies an NV12 texture into the staging texture and returns its bytes, tightly packed
    /// (Y plane, then interleaved UV plane).
    fn read_nv12(&self, slot: &ID3D11Texture2D) -> io::Result<Vec<u8>> {
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| io::Error::other("software staging texture missing"))?;
        // SAFETY: staging and slot are live NV12 textures of the same size on self.device.
        unsafe { self.context.CopyResource(staging, slot) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging is a READ-capable staging texture; mapped is a valid out-parameter.
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped as *mut _))
        }
        .map_err(ctx_err("map NV12 staging texture"))?;
        let (w, h) = (self.canvas_w as usize, self.canvas_h as usize);
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        // SAFETY: while mapped, the NV12 layout is h rows of Y then h/2 rows of interleaved UV,
        // all at RowPitch stride, with the UV plane starting at base + pitch * h. Every row read
        // is w bytes, which RowPitch (>= w) covers.
        unsafe {
            for y in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts(base.add(y * pitch), w));
            }
            let uv = base.add(pitch * h);
            for y in 0..h / 2 {
                out.extend_from_slice(std::slice::from_raw_parts(uv.add(y * pitch), w));
            }
        }
        // SAFETY: balances the successful Map above; no pointer into the mapping is used after.
        unsafe { self.context.Unmap(staging, 0) };
        Ok(out)
    }
}

/// Encoder candidates in try order: matching-vendor hardware, other hardware (only when a vendor
/// is wanted), then software encoders.
fn ordered_candidates(
    list: &[EncoderDescriptor],
    vendor: Option<Vendor>,
) -> Vec<&EncoderDescriptor> {
    let mut out = Vec::new();
    if let Some(vendor) = vendor {
        out.extend(list.iter().filter(|d| d.hardware && d.vendor == vendor));
        out.extend(list.iter().filter(|d| d.hardware && d.vendor != vendor));
    }
    out.extend(list.iter().filter(|d| !d.hardware));
    out
}

/// Encodes one frame and muxes every packet it produced.
fn encode_and_mux(
    encoder: &mut dyn VideoEncoder,
    muxer: &mut SegmentMuxer,
    frame: InputFrame<'_>,
    pts: i64,
    force: bool,
) -> io::Result<()> {
    let packets = encoder
        .encode(frame, pts, force)
        .map_err(ctx_err("encode video frame"))?;
    mux_video(muxer, packets)
}

/// Hands encoder packets to the muxer.
fn mux_video(muxer: &mut SegmentMuxer, packets: Vec<EncodedPacket>) -> io::Result<()> {
    for p in packets {
        muxer
            .push_video(VideoPacket {
                data: p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .map_err(mp4_err)?;
    }
    Ok(())
}

/// Creates a BGRA texture usable as a converter source (DEFAULT usage, optional initial data
/// given as `(bytes, row pitch)`).
fn create_bgra(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    init: Option<(&[u8], u32)>,
) -> io::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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
    let data = init.map(|(bytes, pitch)| D3D11_SUBRESOURCE_DATA {
        pSysMem: bytes.as_ptr().cast(),
        SysMemPitch: pitch,
        SysMemSlicePitch: 0,
    });
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: desc and data (when present) are valid locals that outlive the call; the bytes
    // behind data.pSysMem are borrowed from `init` and at least pitch * height long by the caller.
    unsafe {
        device.CreateTexture2D(
            &desc as *const _,
            data.as_ref().map(|d| d as *const _),
            Some(&mut texture as *mut _),
        )
    }
    .map_err(ctx_err("CreateTexture2D BGRA"))?;
    texture.ok_or_else(|| io::Error::other("CreateTexture2D returned no BGRA texture"))
}

/// Creates the NV12 staging texture used for software read-back.
fn create_staging_nv12(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> io::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: desc is a valid local descriptor; the out-pointer references a live Option slot
    // that the API fills with a COM reference on success.
    unsafe { device.CreateTexture2D(&desc as *const _, None, Some(&mut texture as *mut _)) }
        .map_err(ctx_err("CreateTexture2D NV12 staging"))?;
    texture.ok_or_else(|| io::Error::other("CreateTexture2D returned no staging texture"))
}

fn bad_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn ctx_err<E: Display>(context: &'static str) -> impl Fn(E) -> io::Error {
    move |e| io::Error::other(format!("{context}: {e}"))
}

fn mp4_err(e: clipcheck_mp4::Mp4Error) -> io::Error {
    io::Error::other(format!("mp4 mux: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_encoder_names() {
        assert_eq!(
            native_target("h264_nvenc"),
            Some(NativeTarget {
                codec: Codec::H264,
                vendor: Some(Vendor::Nvidia)
            })
        );
        assert_eq!(
            native_target("libx264"),
            Some(NativeTarget {
                codec: Codec::H264,
                vendor: None
            })
        );
        assert_eq!(native_target("av1_nvenc"), None);
        assert_eq!(native_target("libx265"), None);
        assert_eq!(native_target(""), None);
    }

    #[test]
    fn native_smoke_writes_segments() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let params = NativeParams {
            out_dir: dir.path().join("out"),
            width: 1280,
            height: 720,
            fps: 30,
            target: NativeTarget {
                codec: Codec::H264,
                vendor: Some(Vendor::Nvidia),
            },
            rate: RateControl::Cbr { kbps: 6000 },
            quality_vs_speed: 50,
            segment_secs: 1.0,
            wall_clock: false,
        };
        let audio = [AudioTrackSpec {
            title: "Game".into(),
            bitrate_kbps: 160,
        }];
        let mut enc = NativeEncoder::start(params, None, &audio)?;
        println!("encoder: {}", enc.encoder_name());

        let (w, h) = (1280usize, 720usize);
        let mut frame = vec![0u8; w * h * 4];
        for f in 0..90usize {
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    frame[i] = ((x + f * 4) & 255) as u8;
                    frame[i + 1] = (y & 255) as u8;
                    frame[i + 2] = ((f * 2) & 255) as u8;
                    frame[i + 3] = 255;
                }
            }
            enc.push_bgra(&frame, 1280, 720)?;
            let mut samples = vec![0f32; 3200];
            for i in 0..1600usize {
                let t = (f * 1600 + i) as f32 / 48_000.0;
                let s = (t * 2.0 * std::f32::consts::PI * 440.0).sin() * 0.2;
                samples[2 * i] = s;
                samples[2 * i + 1] = s;
            }
            enc.push_audio(0, &samples)?;
        }

        let report = enc.finish()?;
        println!("{report:?}");
        assert_eq!(report.frames_in, 90);
        assert!(
            report.segments.len() >= 2,
            "segments: {:?}",
            report.segments
        );
        assert!(dir.path().join("out").join("segments.csv").exists());
        Ok(())
    }
}
