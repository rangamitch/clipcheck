//! Synchronous software MFT encoder (Microsoft H.264) fed with system-memory NV12 (worker H3).

use std::mem::ManuallyDrop;
use std::ptr;

use windows::core::{Interface, VARIANT};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI, IMFAttributes, IMFMediaBuffer, IMFSample,
    IMFTransform, MFCreateMemoryBuffer, MFCreateSample, MFVideoFormat_NV12,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_SUBTYPE,
};

use crate::mf_common::{self, variant_u32};
use crate::mf_enum;
use crate::{
    EncError, EncodedPacket, EncoderDescriptor, InputFrame, VideoEncoder, VideoEncoderConfig,
};

/// Minimum output buffer for MFTs that do not provide their own samples.
const MIN_OUTPUT_BUFFER: u32 = 1 << 20;
/// Stream-change renegotiations tolerated within one drain before giving up.
const MAX_STREAM_CHANGES: u32 = 4;

/// Software encoder session.
pub(crate) struct MfSyncEncoder {
    desc: EncoderDescriptor,
    cfg: VideoEncoderConfig,
    transform: IMFTransform,
    codec_api: Option<ICodecAPI>,
    /// Sequence header (SPS/PPS) of the current output type; prepended to keyframes lacking one.
    seq_header: Vec<u8>,
    /// Whether the MFT allocates its own output samples.
    provides_samples: bool,
    /// Output buffer size used when the MFT does not provide samples.
    out_size: u32,
    /// Duration of one frame in 100 ns units.
    frame_duration: i64,
}

// SAFETY: the encoder is created in the MTA (`mf_enum::startup` joins it) and is only ever used
// from one thread at a time, through `&mut self`; the COM pointers it holds are not aliased.
unsafe impl Send for MfSyncEncoder {}

/// Same rules as the hardware encoder: even non-zero size, fps > 0, GOP > 0.
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

impl MfSyncEncoder {
    /// Opens the software MFT for `desc`.
    pub(crate) fn open(
        desc: &EncoderDescriptor,
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
        if let Ok(attrs) = unsafe { transform.GetAttributes() } {
            Self::set_low_latency(&attrs, cfg);
        }

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

        let (provides_samples, out_size) = Self::output_stream_info(&transform)?;

        unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0) }
            .map_err(EncError::win("NOTIFY_BEGIN_STREAMING"))?;
        // SAFETY: as above.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0) }
            .map_err(EncError::win("NOTIFY_START_OF_STREAM"))?;

        let seq_header = mf_common::sequence_header(&transform);
        Ok(Self {
            desc: desc.clone(),
            cfg: *cfg,
            transform,
            codec_api,
            seq_header,
            provides_samples,
            out_size,
            frame_duration: 10_000_000 / i64::from(cfg.fps),
        })
    }

    /// Sets `MF_LOW_LATENCY` on the transform's attribute store. A hint only: failures are ignored.
    fn set_low_latency(attrs: &IMFAttributes, cfg: &VideoEncoderConfig) {
        // SAFETY: the key is a static MF GUID; the call only reads it.
        let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, u32::from(cfg.low_latency)) };
    }

    /// Whether the output stream provides its samples, and the buffer size to allocate otherwise.
    fn output_stream_info(transform: &IMFTransform) -> Result<(bool, u32), EncError> {
        // SAFETY: GetOutputStreamInfo takes no pointers from us.
        let info = unsafe { transform.GetOutputStreamInfo(0) }
            .map_err(EncError::win("GetOutputStreamInfo"))?;
        let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        Ok((provides, info.cbSize.max(MIN_OUTPUT_BUFFER)))
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

    /// Copies one NV12 frame into a new input sample stamped with `pts`.
    fn input_sample(&self, data: &[u8], pts: i64) -> Result<IMFSample, EncError> {
        let len =
            u32::try_from(data.len()).map_err(|_| EncError::BadInput("frame too large".into()))?;
        // SAFETY: MFCreateMemoryBuffer takes a plain size.
        let buffer: IMFMediaBuffer =
            unsafe { MFCreateMemoryBuffer(len) }.map_err(EncError::win("MFCreateMemoryBuffer"))?;
        let mut dst: *mut u8 = ptr::null_mut();
        // SAFETY: Lock writes a pointer to the buffer's `len` writable bytes into `dst`; the copy
        // below stays inside that range and happens before Unlock.
        unsafe { buffer.Lock(&mut dst, None, None) }
            .map_err(EncError::win("IMFMediaBuffer::Lock"))?;
        let copied = if dst.is_null() {
            Err(EncError::Windows {
                context: "IMFMediaBuffer::Lock",
                message: "null buffer pointer".into(),
                hresult: 0,
            })
        } else {
            // SAFETY: `data` holds `len` bytes, `dst` is valid for `len` bytes while locked, and
            // the two ranges cannot overlap (one is our input slice, the other a fresh buffer).
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) };
            Ok(())
        };
        // SAFETY: balances the successful Lock above; `dst` is not used after this point.
        let _ = unsafe { buffer.Unlock() };
        copied?;
        // SAFETY: the buffer is live and `len` does not exceed its capacity.
        unsafe { buffer.SetCurrentLength(len) }.map_err(EncError::win("SetCurrentLength"))?;
        // SAFETY: MFCreateSample takes no pointers.
        let sample = unsafe { MFCreateSample() }.map_err(EncError::win("MFCreateSample"))?;
        // SAFETY: both objects are live; AddBuffer takes its own reference.
        unsafe { sample.AddBuffer(&buffer) }.map_err(EncError::win("AddBuffer"))?;
        // SAFETY: plain value arguments.
        unsafe { sample.SetSampleTime(pts) }.map_err(EncError::win("SetSampleTime"))?;
        // SAFETY: plain value argument.
        unsafe { sample.SetSampleDuration(self.frame_duration) }
            .map_err(EncError::win("SetSampleDuration"))?;
        Ok(sample)
    }

    /// A fresh output sample for MFTs that do not provide their own.
    fn output_sample(&self) -> Result<IMFSample, EncError> {
        // SAFETY: MFCreateMemoryBuffer takes a plain size.
        let buffer = unsafe { MFCreateMemoryBuffer(self.out_size) }
            .map_err(EncError::win("MFCreateMemoryBuffer (output)"))?;
        // SAFETY: MFCreateSample takes no pointers.
        let sample =
            unsafe { MFCreateSample() }.map_err(EncError::win("MFCreateSample (output)"))?;
        // SAFETY: both objects are live; AddBuffer takes its own reference.
        unsafe { sample.AddBuffer(&buffer) }.map_err(EncError::win("AddBuffer (output)"))?;
        Ok(sample)
    }

    /// Sets (`on` = 1) or clears (`on` = 0) the one-shot force-keyframe request. Failures are
    /// ignored: the frame is then simply not forced.
    fn set_force_keyframe(&self, on: u32) {
        if let Some(api) = &self.codec_api {
            let value: VARIANT = variant_u32(on);
            // SAFETY: the key is a static GUID and `value` is a live VARIANT for the call.
            let _ = unsafe {
                api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value as *const VARIANT)
            };
        }
    }

    /// Pulls every ready output into `out`, until the MFT asks for more input.
    fn drain(&mut self, out: &mut Vec<EncodedPacket>) -> Result<(), EncError> {
        let mut stream_changes = 0u32;
        loop {
            let own_sample = if self.provides_samples {
                None
            } else {
                Some(self.output_sample()?)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: `buffers` is a valid one-element array; `status` is a live local.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: each ManuallyDrop field is taken exactly once here and the array is never
            // dropped, so the fields are released only through these takes.
            let produced = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
            // SAFETY: as above.
            let events = unsafe { ManuallyDrop::take(&mut buffers[0].pEvents) };
            drop(events);

            match result {
                Ok(()) => match produced {
                    Some(sample) => {
                        out.push(mf_common::sample_to_packet(
                            &sample,
                            &self.seq_header,
                            self.cfg.codec,
                        )?);
                    }
                    None => return Ok(()),
                },
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e)
                    if e.code() == MF_E_TRANSFORM_STREAM_CHANGE
                        && stream_changes < MAX_STREAM_CHANGES =>
                {
                    stream_changes += 1;
                    // SAFETY: plain index arguments.
                    let offer = unsafe { self.transform.GetOutputAvailableType(0, 0) }
                        .map_err(EncError::win("GetOutputAvailableType"))?;
                    // SAFETY: `offer` is a live media type.
                    unsafe { self.transform.SetOutputType(0, &offer, 0) }
                        .map_err(EncError::win("SetOutputType (stream change)"))?;
                    self.seq_header = mf_common::sequence_header(&self.transform);
                    let (provides, size) = Self::output_stream_info(&self.transform)?;
                    self.provides_samples = provides;
                    self.out_size = size;
                }
                Err(e) => return Err(EncError::win("ProcessOutput")(e)),
            }
        }
    }
}

impl VideoEncoder for MfSyncEncoder {
    fn encode(
        &mut self,
        frame: InputFrame<'_>,
        pts: i64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedPacket>, EncError> {
        let data = match frame {
            InputFrame::Nv12 {
                data,
                width,
                height,
            } => {
                if width != self.cfg.width || height != self.cfg.height {
                    return Err(EncError::BadInput(format!(
                        "frame {width}x{height} does not match the session {}x{}",
                        self.cfg.width, self.cfg.height
                    )));
                }
                let expected = width as usize * height as usize * 3 / 2;
                if data.len() != expected {
                    return Err(EncError::BadInput(format!(
                        "NV12 frame is {} bytes, expected {expected}",
                        data.len()
                    )));
                }
                data
            }
            InputFrame::Texture(_) => {
                return Err(EncError::BadInput(
                    "software encoder needs system-memory NV12".into(),
                ));
            }
        };

        let sample = self.input_sample(data, pts)?;
        let mut out = Vec::new();
        if force_keyframe {
            self.set_force_keyframe(1);
        }
        // SAFETY: `sample` is a live sample; ProcessInput only reads it.
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                // The MFT has output waiting: take it, then offer the input again.
                self.drain(&mut out)?;
                // SAFETY: as above.
                unsafe { self.transform.ProcessInput(0, &sample, 0) }
                    .map_err(EncError::win("ProcessInput"))?;
            }
            Err(e) => return Err(EncError::win("ProcessInput")(e)),
        }
        if force_keyframe {
            // One-shot: the request was consumed by this input, so clear it for the next frame.
            self.set_force_keyframe(0);
        }
        self.drain(&mut out)?;
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncError> {
        // SAFETY: the message parameters are plain values.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
        }
        .map_err(EncError::win("NOTIFY_END_OF_STREAM"))?;
        // SAFETY: as above.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(EncError::win("COMMAND_DRAIN"))?;
        let mut out = Vec::new();
        self.drain(&mut out)?;
        Ok(out)
    }

    fn descriptor(&self) -> &EncoderDescriptor {
        &self.desc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mf_common::has_sps;
    use crate::{open, Codec, RateControl};

    /// Software H.264 round trip: 60 synthetic NV12 frames, keyframes forced at 0 and 30.
    #[test]
    fn smoke_software_h264_roundtrip() {
        let descs = crate::enumerate(Codec::H264).expect("enumerate");
        let Some(desc) = descs.into_iter().find(|d| !d.hardware) else {
            eprintln!("SKIPPED: no software H.264 encoder");
            return;
        };
        let cfg = VideoEncoderConfig {
            codec: Codec::H264,
            width: 640,
            height: 360,
            fps: 30,
            rate: RateControl::Cbr { kbps: 2000 },
            gop_frames: 30,
            quality_vs_speed: 50,
            low_latency: false,
        };
        let mut enc: Box<dyn VideoEncoder> = open(&desc, None, &cfg).expect("open");

        let (w, h) = (cfg.width as usize, cfg.height as usize);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        let mut packets: Vec<EncodedPacket> = Vec::new();
        for i in 0..60usize {
            for y in 0..h {
                for x in 0..w {
                    nv12[y * w + x] = ((x + y + i) & 0xFF) as u8;
                }
            }
            let force = i == 0 || i == 30;
            let got = enc
                .encode(
                    InputFrame::Nv12 {
                        data: &nv12,
                        width: cfg.width,
                        height: cfg.height,
                    },
                    i as i64 * 333_333,
                    force,
                )
                .expect("encode");
            packets.extend(got);
        }
        packets.extend(enc.flush().expect("flush"));

        let keyframes = packets.iter().filter(|p| p.keyframe).count();
        let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
        eprintln!(
            "{}: packets={} keyframes={} bytes={}",
            desc.name,
            packets.len(),
            keyframes,
            bytes
        );
        assert!(packets.len() >= 55, "only {} packets", packets.len());
        assert!(packets[0].keyframe, "first packet is not a keyframe");
        assert!(
            has_sps(&packets[0].data, Codec::H264),
            "first packet has no SPS"
        );
    }
}
