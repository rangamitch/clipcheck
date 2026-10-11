//! Asynchronous hardware MFT encoder fed with D3D11 textures (worker H2).

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI, IMF2DBuffer, IMFDXGIDeviceManager,
    IMFMediaEventGenerator, IMFSample, IMFTransform, METransformDrainComplete,
    METransformHaveOutput, METransformNeedInput, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMemoryBuffer, MFCreateSample, MFVideoFormat_NV12,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MF_EVENT_FLAG_NO_WAIT, MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK,
};

use crate::mf_common::{self, variant_u32};
use crate::mf_enum;
use crate::{
    EncError, EncodedPacket, EncoderDescriptor, InputFrame, VideoEncoder, VideoEncoderConfig,
};

/// Input samples queued before the oldest one is dropped.
const MAX_PENDING: usize = 8;
/// Minimum output buffer for MFTs that do not provide their own samples.
const MIN_OUTPUT_BUFFER: u32 = 1 << 20;
/// How long `flush` waits for the drain to finish.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
/// How long `encode` waits for the MFT to take input when the queue is full.
const BACKPRESSURE_WAIT: Duration = Duration::from_millis(200);

/// Hardware encoder session.
pub(crate) struct MfAsyncEncoder {
    desc: EncoderDescriptor,
    cfg: VideoEncoderConfig,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    /// Kept alive: the transform holds a pointer to the manager's device.
    _manager: IMFDXGIDeviceManager,
    /// Input requests the MFT has signalled and we have not yet satisfied.
    need_input: u32,
    /// Frames waiting for the MFT, with their force-keyframe flag.
    pending: VecDeque<(IMFSample, bool)>,
    seq_header: Vec<u8>,
    provides_samples: bool,
    out_size: u32,
    drained: bool,
    dropped: u64,
}

// SAFETY: the MF transform and the D3D11 objects are free-threaded COM objects in the process MTA
// (see mf_enum::startup), and the encoder is used from one thread at a time (`&mut self`).
unsafe impl Send for MfAsyncEncoder {}

/// Rejects settings the encoder cannot use.
fn validate(cfg: &VideoEncoderConfig) -> Result<(), EncError> {
    if cfg.width == 0
        || cfg.height == 0
        || !cfg.width.is_multiple_of(2)
        || !cfg.height.is_multiple_of(2)
    {
        return Err(EncError::BadInput(format!(
            "frame size {}x{} must be even and non-zero",
            cfg.width, cfg.height
        )));
    }
    if cfg.fps == 0 {
        return Err(EncError::BadInput("fps must be > 0".into()));
    }
    if cfg.gop_frames == 0 {
        return Err(EncError::BadInput("gop_frames must be > 0".into()));
    }
    Ok(())
}

impl MfAsyncEncoder {
    /// Opens the hardware MFT for `desc` on `device`.
    pub(crate) fn open(
        desc: &EncoderDescriptor,
        device: &ID3D11Device,
        cfg: &VideoEncoderConfig,
    ) -> Result<Self, EncError> {
        validate(cfg)?;
        if cfg.codec != desc.codec {
            return Err(EncError::BadInput(format!(
                "{} does not produce {:?}",
                desc.name, cfg.codec
            )));
        }
        mf_enum::startup()?;
        let activate = mf_enum::activate_for(desc)?;
        // SAFETY: `activate` is a live IMFActivate; ActivateObject creates the transform.
        let transform: IMFTransform = unsafe { activate.ActivateObject::<IMFTransform>() }
            .map_err(EncError::win("ActivateObject"))?;

        // SAFETY: GetAttributes returns a live attribute store owned by the transform.
        let attrs = unsafe { transform.GetAttributes() }
            .map_err(EncError::win("IMFTransform::GetAttributes"))?;
        // SAFETY: the key is a static MF GUID; the call only reads it.
        unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
            .map_err(EncError::win("MF_TRANSFORM_ASYNC_UNLOCK"))?;
        // SAFETY: as above; low latency is a hint, so a failure is ignored.
        let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, u32::from(cfg.low_latency)) };

        let mut token = 0u32;
        let mut manager_out: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: both out-pointers reference live locals of the right types.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager_out) }
            .map_err(EncError::win("MFCreateDXGIDeviceManager"))?;
        let manager = manager_out.ok_or_else(|| EncError::Windows {
            context: "MFCreateDXGIDeviceManager",
            message: "no manager returned".into(),
            hresult: 0,
        })?;
        // SAFETY: `device` is a live D3D11 device; the token came from the call above.
        unsafe { manager.ResetDevice(device, token) }
            .map_err(EncError::win("IMFDXGIDeviceManager::ResetDevice"))?;
        // SAFETY: the manager pointer is valid while `manager` lives, which is as long as the
        // transform (both are owned by the returned encoder).
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }
            .map_err(EncError::win("MFT_MESSAGE_SET_D3D_MANAGER"))?;

        let codec_api = transform.cast::<ICodecAPI>().ok();
        if let Some(api) = &codec_api {
            // Individual setting failures are non-fatal; the encoder runs with defaults.
            let _failed = mf_common::set_codec_api(api, cfg);
        }

        // Output (compressed) type first: MFTs refuse input types they cannot pair with it.
        let out_type = mf_common::output_type(cfg)?;
        // SAFETY: `out_type` is a live media type.
        unsafe { transform.SetOutputType(0, &out_type, 0) }
            .map_err(EncError::win("SetOutputType"))?;
        let in_type = mf_common::input_type_nv12(cfg)?;
        // SAFETY: `in_type` is a live media type.
        if unsafe { transform.SetInputType(0, &in_type, 0) }.is_err() {
            Self::set_input_from_available(&transform, cfg)?;
        }

        // SAFETY: GetOutputStreamInfo takes no pointers from us.
        let info = unsafe { transform.GetOutputStreamInfo(0) }
            .map_err(EncError::win("GetOutputStreamInfo"))?;
        let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        let out_size = info.cbSize.max(MIN_OUTPUT_BUFFER);

        // SAFETY: the message parameters are plain values; the MFT ignores the unused ones.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }
            .map_err(EncError::win("COMMAND_FLUSH"))?;
        // SAFETY: as above.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0) }
            .map_err(EncError::win("NOTIFY_BEGIN_STREAMING"))?;
        // SAFETY: as above.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0) }
            .map_err(EncError::win("NOTIFY_START_OF_STREAM"))?;

        let seq_header = mf_common::sequence_header(&transform);
        let events = transform
            .cast::<IMFMediaEventGenerator>()
            .map_err(EncError::win("IMFMediaEventGenerator"))?;

        Ok(Self {
            desc: desc.clone(),
            cfg: *cfg,
            transform,
            events,
            codec_api,
            _manager: manager,
            need_input: 0,
            pending: VecDeque::new(),
            seq_header,
            provides_samples,
            out_size,
            drained: false,
            dropped: 0,
        })
    }

    /// Fallback when the MFT rejects the plain NV12 type: take its own NV12 offer and set the
    /// frame size and rate on it.
    fn set_input_from_available(
        transform: &IMFTransform,
        cfg: &VideoEncoderConfig,
    ) -> Result<(), EncError> {
        for index in 0..32 {
            // SAFETY: the call takes plain integers; it fails with MF_E_NO_MORE_TYPES past the end.
            let Ok(offer) = (unsafe { transform.GetInputAvailableType(0, index) }) else {
                break;
            };
            // SAFETY: `offer` is a live media type; the key is a static MF GUID.
            let subtype = unsafe { offer.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default();
            if subtype != MFVideoFormat_NV12 {
                continue;
            }
            // SAFETY: `offer` is live; the keys are static MF GUIDs.
            unsafe {
                offer
                    .SetUINT64(&MF_MT_FRAME_SIZE, mf_common::frame_size_value(cfg))
                    .map_err(EncError::win("MF_MT_FRAME_SIZE"))?;
                offer
                    .SetUINT64(&MF_MT_FRAME_RATE, mf_common::frame_rate_value(cfg))
                    .map_err(EncError::win("MF_MT_FRAME_RATE"))?;
                transform
                    .SetInputType(0, &offer, 0)
                    .map_err(EncError::win("SetInputType"))?;
            }
            return Ok(());
        }
        Err(EncError::NotFound(
            "no NV12 input type accepted by the encoder".into(),
        ))
    }

    /// Wraps an NV12 texture in an input sample without copying it.
    fn texture_sample(&self, texture: &ID3D11Texture2D, pts: i64) -> Result<IMFSample, EncError> {
        // SAFETY: `texture` is a live D3D11 texture on the device the manager was reset to.
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false) }
            .map_err(EncError::win("MFCreateDXGISurfaceBuffer"))?;
        // SAFETY: `buffer` wraps a 2D surface, so the IMF2DBuffer query is valid.
        let length = unsafe {
            let two_d: IMF2DBuffer = buffer.cast().map_err(EncError::win("IMF2DBuffer"))?;
            two_d
                .GetContiguousLength()
                .map_err(EncError::win("GetContiguousLength"))?
        };
        // SAFETY: `buffer` is live; the length is the surface's contiguous size.
        unsafe { buffer.SetCurrentLength(length) }.map_err(EncError::win("SetCurrentLength"))?;
        // SAFETY: MFCreateSample takes no pointers.
        let sample = unsafe { MFCreateSample() }.map_err(EncError::win("MFCreateSample"))?;
        // SAFETY: `sample` and `buffer` are live.
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(EncError::win("AddBuffer"))?;
            sample
                .SetSampleTime(pts)
                .map_err(EncError::win("SetSampleTime"))?;
            sample
                .SetSampleDuration(10_000_000 / i64::from(self.cfg.fps))
                .map_err(EncError::win("SetSampleDuration"))?;
        }
        Ok(sample)
    }

    /// An empty output sample for MFTs that do not allocate their own.
    fn output_sample(&self) -> Result<IMFSample, EncError> {
        // SAFETY: MFCreateMemoryBuffer takes a plain size.
        let buffer = unsafe { MFCreateMemoryBuffer(self.out_size) }
            .map_err(EncError::win("MFCreateMemoryBuffer"))?;
        // SAFETY: MFCreateSample takes no pointers.
        let sample = unsafe { MFCreateSample() }.map_err(EncError::win("MFCreateSample"))?;
        // SAFETY: both objects are live.
        unsafe { sample.AddBuffer(&buffer) }.map_err(EncError::win("AddBuffer"))?;
        Ok(sample)
    }

    /// Takes every event that is ready now (non-blocking).
    fn drain_events(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncError> {
        loop {
            // SAFETY: the generator is live; NO_WAIT never blocks.
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(EncError::win("IMFMediaEventGenerator::GetEvent")(e)),
            };
            // SAFETY: the event is live.
            let kind =
                unsafe { event.GetType() }.map_err(EncError::win("IMFMediaEvent::GetType"))?;
            if kind == METransformNeedInput.0 as u32 {
                self.need_input += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                self.process_output(out)?;
            } else if kind == METransformDrainComplete.0 as u32 {
                self.drained = true;
            }
        }
    }

    /// Submits queued frames while the MFT wants input.
    fn feed(&mut self) -> Result<(), EncError> {
        while self.need_input > 0 {
            let Some((sample, force_keyframe)) = self.pending.pop_front() else {
                break;
            };
            if force_keyframe {
                if let Some(api) = &self.codec_api {
                    let value = variant_u32(1);
                    // SAFETY: the key is a static GUID and `value` lives for the call.
                    // Failure only means the keyframe is not forced; the frame still encodes.
                    let _ = unsafe {
                        api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value as *const _)
                    };
                }
            }
            // SAFETY: `sample` is live and the MFT keeps its own reference once accepted.
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => self.need_input -= 1,
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    // Not ready: keep the frame and wait for the next NeedInput event.
                    self.pending.push_front((sample, force_keyframe));
                    self.need_input = 0;
                    break;
                }
                Err(e) => return Err(EncError::win("ProcessInput")(e)),
            }
        }
        Ok(())
    }

    /// Pulls one output sample from the MFT, handling stream changes.
    fn process_output(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncError> {
        let mut retried = false;
        loop {
            let sample = if self.provides_samples {
                None
            } else {
                Some(self.output_sample()?)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: `buffers` is a valid one-element array; `status` is a live local.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: the fields are taken exactly once here and never used again; `buffers`
            // is not dropped, so the ManuallyDrop fields are released only through these takes.
            let produced = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
            // SAFETY: as above.
            let events = unsafe { ManuallyDrop::take(&mut buffers[0].pEvents) };
            drop(events);

            match result {
                Ok(()) => {
                    if let Some(sample) = produced {
                        out.push(mf_common::sample_to_packet(
                            &sample,
                            &self.seq_header,
                            self.cfg.codec,
                        )?);
                    }
                    return Ok(());
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE && !retried => {
                    retried = true;
                    // SAFETY: plain index arguments.
                    let offer = unsafe { self.transform.GetOutputAvailableType(0, 0) }
                        .map_err(EncError::win("GetOutputAvailableType"))?;
                    // SAFETY: `offer` is a live media type.
                    unsafe { self.transform.SetOutputType(0, &offer, 0) }
                        .map_err(EncError::win("SetOutputType (stream change)"))?;
                    self.seq_header = mf_common::sequence_header(&self.transform);
                }
                Err(e) => return Err(EncError::win("ProcessOutput")(e)),
            }
        }
    }

    /// Event and feed loop: collects every packet that is ready after one pass.
    fn pump(&mut self) -> Result<Vec<EncodedPacket>, EncError> {
        let mut out = Vec::new();
        self.drain_events(&mut out)?;
        self.feed()?;
        self.drain_events(&mut out)?;
        Ok(out)
    }
}

impl VideoEncoder for MfAsyncEncoder {
    fn encode(
        &mut self,
        frame: InputFrame<'_>,
        pts: i64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedPacket>, EncError> {
        let InputFrame::Texture(texture) = frame else {
            return Err(EncError::BadInput(
                "the hardware encoder takes D3D11 textures only".into(),
            ));
        };
        let sample = self.texture_sample(texture, pts)?;
        self.pending.push_back((sample, force_keyframe));
        let mut out = self.pump()?;
        // Backpressure: give the encoder a short window to take input before dropping a frame.
        let deadline = Instant::now() + BACKPRESSURE_WAIT;
        while self.pending.len() > MAX_PENDING && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
            self.drain_events(&mut out)?;
            self.feed()?;
        }
        if self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
            self.dropped += 1;
        }
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncError> {
        let mut out = Vec::new();
        let deadline = Instant::now() + DRAIN_TIMEOUT;

        // Submit whatever is still queued before the end-of-stream signal.
        while !self.pending.is_empty() && Instant::now() < deadline {
            self.drain_events(&mut out)?;
            self.feed()?;
            if !self.pending.is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
        }
        if !self.pending.is_empty() {
            self.dropped += self.pending.len() as u64;
            self.pending.clear();
        }

        // SAFETY: plain message values; neither takes a pointer from us.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
        }
        .map_err(EncError::win("NOTIFY_END_OF_STREAM"))?;
        // SAFETY: as above.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(EncError::win("COMMAND_DRAIN"))?;

        self.drained = false;
        while !self.drained && Instant::now() < deadline {
            self.drain_events(&mut out)?;
            if !self.drained {
                thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(out)
    }

    fn descriptor(&self) -> &EncoderDescriptor {
        &self.desc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{enumerate, Codec, Vendor};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

    const W: u32 = 1280;
    const H: u32 = 720;
    const FRAMES: i64 = 90;

    /// Builds one NV12 texture whose luma varies with `x`, `y` and `frame`.
    fn nv12_texture(device: &ID3D11Device, frame: u32) -> Result<ID3D11Texture2D, String> {
        let y_len = (W * H) as usize;
        let mut data = vec![128u8; y_len * 3 / 2];
        for y in 0..H {
            for x in 0..W {
                data[(y * W + x) as usize] = ((x + y + frame) & 0xFF) as u8;
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: W,
            Height: H,
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
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: data.as_ptr() as *const _,
            SysMemPitch: W,
            SysMemSlicePitch: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` and `init` are valid for the call; `data` outlives it and the driver
        // copies the initial contents during creation.
        unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut texture)) }
            .map_err(|e| format!("CreateTexture2D: {e}"))?;
        texture.ok_or_else(|| "CreateTexture2D returned no texture".to_string())
    }

    #[test]
    fn hardware_h264_round_trip() {
        let (device, _context) = match clipcheck_gpuconv::d3d::create_device(None) {
            Ok(pair) => pair,
            Err(e) => {
                println!("SKIPPED: no D3D11 device: {e:?}");
                return;
            }
        };
        let descs = match enumerate(Codec::H264) {
            Ok(list) => list,
            Err(e) => panic!("enumerate failed: {e}"),
        };
        let hardware: Vec<_> = descs.iter().filter(|d| d.hardware).collect();
        let desc = hardware
            .iter()
            .find(|d| d.vendor == Vendor::Nvidia)
            .or(hardware.first())
            .copied();
        let Some(desc) = desc else {
            println!("SKIPPED: no hardware H.264 encoder");
            return;
        };
        println!("encoder: {} ({:?})", desc.name, desc.vendor);

        let cfg = VideoEncoderConfig {
            codec: Codec::H264,
            width: W,
            height: H,
            fps: 30,
            rate: crate::RateControl::Cbr { kbps: 6000 },
            gop_frames: 60,
            quality_vs_speed: 50,
            low_latency: false,
        };
        let mut encoder = MfAsyncEncoder::open(desc, &device, &cfg).expect("open hardware encoder");

        let textures: Vec<ID3D11Texture2D> = (0..4)
            .map(|i| nv12_texture(&device, i).expect("NV12 texture"))
            .collect();

        let mut packets = Vec::new();
        for i in 0..FRAMES {
            let texture = &textures[(i % 4) as usize];
            let key = i == 0 || i == 60;
            let got = encoder
                .encode(InputFrame::Texture(texture), i * 333_333, key)
                .expect("encode");
            packets.extend(got);
        }
        packets.extend(encoder.flush().expect("flush"));

        let keyframes = packets.iter().filter(|p| p.keyframe).count();
        let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
        println!(
            "packets={} keyframes={} bytes={} dropped={}",
            packets.len(),
            keyframes,
            bytes,
            encoder.dropped
        );
        assert!(packets.len() >= 80, "only {} packets", packets.len());
        let first = &packets[0];
        assert!(first.keyframe, "first packet is not a keyframe");
        assert!(
            mf_common::has_sps(&first.data, Codec::H264),
            "first keyframe has no SPS"
        );
    }
}
