//! Media type setup, ICodecAPI settings and sample conversion shared by both MF encoders (worker H2).

use std::ptr;

use windows::core::imp;
use windows::core::{GUID, VARIANT};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQuality,
    CODECAPI_AVEncCommonQualityVsSpeed, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVLowLatencyMode,
    ICodecAPI, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MFCreateMediaType,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFSampleExtension_DecodeTimestamp,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
};
use windows::Win32::System::Variant::{VT_BOOL, VT_UI4};

use crate::{Codec, EncError, EncodedPacket, RateControl, VideoEncoderConfig};

/// A `VT_UI4` variant holding `v`.
pub(crate) fn variant_u32(v: u32) -> VARIANT {
    // SAFETY: an all-zero VARIANT is a valid VT_EMPTY value.
    let mut raw: imp::VARIANT = unsafe { std::mem::zeroed() };
    raw.Anonymous.Anonymous.vt = VT_UI4.0;
    raw.Anonymous.Anonymous.Anonymous.ulVal = v;
    // SAFETY: the raw value is a plain VT_UI4 with no heap-owned payload, so the wrapper's
    // VariantClear on drop has nothing to free.
    unsafe { VARIANT::from_raw(raw) }
}

/// A `VT_BOOL` variant (`VARIANT_TRUE` = -1, `VARIANT_FALSE` = 0).
pub(crate) fn variant_bool(b: bool) -> VARIANT {
    // SAFETY: an all-zero VARIANT is a valid VT_EMPTY value.
    let mut raw: imp::VARIANT = unsafe { std::mem::zeroed() };
    raw.Anonymous.Anonymous.vt = VT_BOOL.0;
    raw.Anonymous.Anonymous.Anonymous.boolVal = if b { -1 } else { 0 };
    // SAFETY: a plain VT_BOOL carries no heap-owned payload.
    unsafe { VARIANT::from_raw(raw) }
}

/// One `ICodecAPI::SetValue`; true when it succeeded.
fn set_value(api: &ICodecAPI, key: &GUID, value: VARIANT) -> bool {
    // SAFETY: `key` and `value` are valid for the duration of the call; the API only reads them.
    unsafe { api.SetValue(key, &value as *const VARIANT) }.is_ok()
}

/// Applies the rate, GOP, quality and latency settings. Never fails: every failing `SetValue`
/// is ignored and counted in the return value.
pub(crate) fn set_codec_api(api: &ICodecAPI, cfg: &VideoEncoderConfig) -> u32 {
    let mut ops: Vec<(&GUID, VARIANT)> = Vec::new();
    match cfg.rate {
        RateControl::Cbr { kbps } => {
            ops.push((&CODECAPI_AVEncCommonRateControlMode, variant_u32(0)));
            ops.push((
                &CODECAPI_AVEncCommonMeanBitRate,
                variant_u32(kbps.saturating_mul(1000)),
            ));
        }
        RateControl::Vbr { kbps, peak_kbps } => {
            ops.push((&CODECAPI_AVEncCommonRateControlMode, variant_u32(2)));
            ops.push((
                &CODECAPI_AVEncCommonMeanBitRate,
                variant_u32(kbps.saturating_mul(1000)),
            ));
            ops.push((
                &CODECAPI_AVEncCommonMaxBitRate,
                variant_u32(peak_kbps.saturating_mul(1000)),
            ));
        }
        RateControl::Quality { cq } => {
            let quality = (100 - u32::from(cq) * 100 / 51).clamp(1, 100);
            ops.push((&CODECAPI_AVEncCommonRateControlMode, variant_u32(3)));
            ops.push((&CODECAPI_AVEncCommonQuality, variant_u32(quality)));
        }
    }
    ops.push((&CODECAPI_AVEncMPVGOPSize, variant_u32(cfg.gop_frames)));
    ops.push((&CODECAPI_AVEncMPVDefaultBPictureCount, variant_u32(0)));
    ops.push((
        &CODECAPI_AVEncCommonQualityVsSpeed,
        variant_u32(cfg.quality_vs_speed.min(100)),
    ));
    ops.push((&CODECAPI_AVLowLatencyMode, variant_bool(cfg.low_latency)));

    ops.into_iter()
        .filter(|(key, value)| !set_value(api, key, value.clone()))
        .count() as u32
}

/// `MF_MT_FRAME_SIZE` / `MF_MT_FRAME_RATE` / `MF_MT_PIXEL_ASPECT_RATIO` packed values.
pub(crate) fn frame_size_value(cfg: &VideoEncoderConfig) -> u64 {
    (u64::from(cfg.width) << 32) | u64::from(cfg.height)
}

/// `MF_MT_FRAME_RATE` packed value.
pub(crate) fn frame_rate_value(cfg: &VideoEncoderConfig) -> u64 {
    (u64::from(cfg.fps) << 32) | 1
}

/// Sets the attributes every video media type needs (size, rate, PAR, progressive).
fn set_video_common(t: &IMFMediaType, cfg: &VideoEncoderConfig) -> Result<(), EncError> {
    // SAFETY: `t` is a live media type; the keys are static MF GUIDs read by the call.
    unsafe {
        t.SetUINT64(&MF_MT_FRAME_SIZE, frame_size_value(cfg))
            .map_err(EncError::win("MF_MT_FRAME_SIZE"))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, frame_rate_value(cfg))
            .map_err(EncError::win("MF_MT_FRAME_RATE"))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(EncError::win("MF_MT_PIXEL_ASPECT_RATIO"))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(EncError::win("MF_MT_INTERLACE_MODE"))?;
    }
    Ok(())
}

/// The compressed output media type for `cfg`.
pub(crate) fn output_type(cfg: &VideoEncoderConfig) -> Result<IMFMediaType, EncError> {
    // SAFETY: MFCreateMediaType takes no pointers.
    let t = unsafe { MFCreateMediaType() }.map_err(EncError::win("MFCreateMediaType"))?;
    let (subtype, profile) = match cfg.codec {
        Codec::H264 => (MFVideoFormat_H264, 100u32),
        Codec::Hevc => (MFVideoFormat_HEVC, 1u32),
    };
    let avg_kbps = match cfg.rate {
        RateControl::Cbr { kbps } | RateControl::Vbr { kbps, .. } => kbps,
        RateControl::Quality { .. } => 20_000,
    };
    // SAFETY: `t` is a live media type; all keys are static MF GUIDs.
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(EncError::win("MF_MT_MAJOR_TYPE"))?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)
            .map_err(EncError::win("MF_MT_SUBTYPE"))?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, avg_kbps.saturating_mul(1000))
            .map_err(EncError::win("MF_MT_AVG_BITRATE"))?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)
            .map_err(EncError::win("MF_MT_MPEG2_PROFILE"))?;
    }
    set_video_common(&t, cfg)?;
    Ok(t)
}

/// The uncompressed NV12 input media type for `cfg`.
pub(crate) fn input_type_nv12(cfg: &VideoEncoderConfig) -> Result<IMFMediaType, EncError> {
    // SAFETY: MFCreateMediaType takes no pointers.
    let t = unsafe { MFCreateMediaType() }.map_err(EncError::win("MFCreateMediaType"))?;
    // SAFETY: `t` is a live media type; all keys are static MF GUIDs.
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(EncError::win("MF_MT_MAJOR_TYPE"))?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(EncError::win("MF_MT_SUBTYPE"))?;
    }
    set_video_common(&t, cfg)?;
    Ok(t)
}

/// Whether an Annex B buffer contains an SPS (H.264 type 7, HEVC type 32) NAL unit.
pub(crate) fn has_sps(data: &[u8], codec: Codec) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let b = data[i + 3];
            let is_sps = match codec {
                Codec::H264 => b & 0x1F == 7,
                Codec::Hevc => (b >> 1) & 0x3F == 32,
            };
            if is_sps {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Copies the bytes of a contiguous media buffer.
fn buffer_bytes(buffer: &IMFMediaBuffer) -> Result<Vec<u8>, EncError> {
    let mut ptr_out: *mut u8 = ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: Lock writes a pointer to `len` readable bytes into `ptr_out` and the current
    // length into `len`; both out-pointers reference live locals.
    unsafe { buffer.Lock(&mut ptr_out, None, Some(&mut len as *mut u32)) }
        .map_err(EncError::win("IMFMediaBuffer::Lock"))?;
    let bytes = if ptr_out.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: between a successful Lock and Unlock the buffer exposes `len` valid bytes.
        unsafe { std::slice::from_raw_parts(ptr_out, len as usize) }.to_vec()
    };
    // SAFETY: balances the successful Lock above; no pointer from it is used afterwards.
    let _ = unsafe { buffer.Unlock() };
    Ok(bytes)
}

/// Converts an encoder output sample to an Annex B packet. On a keyframe without an SPS in the
/// payload, `seq_header` is prepended when non-empty.
pub(crate) fn sample_to_packet(
    sample: &IMFSample,
    seq_header: &[u8],
    codec: Codec,
) -> Result<EncodedPacket, EncError> {
    // SAFETY: `sample` is a live sample; the call takes no pointers from us.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(EncError::win("ConvertToContiguousBuffer"))?;
    let mut data = buffer_bytes(&buffer)?;
    // SAFETY: `sample` is live; GetSampleTime takes no pointers.
    let pts = unsafe { sample.GetSampleTime() }.map_err(EncError::win("GetSampleTime"))?;
    // SAFETY: the GUID is a static MF key; the call only reads it.
    let dts = unsafe { sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) }
        .map(|v| v as i64)
        .unwrap_or(pts);
    // SAFETY: the GUID is a static MF key; the call only reads it.
    let keyframe = matches!(
        unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) },
        Ok(1)
    );
    if keyframe && !seq_header.is_empty() && !has_sps(&data, codec) {
        let mut with_header = Vec::with_capacity(seq_header.len() + data.len());
        with_header.extend_from_slice(seq_header);
        with_header.append(&mut data);
        data = with_header;
    }
    Ok(EncodedPacket {
        data,
        pts,
        dts,
        keyframe,
    })
}

/// The `MF_MT_MPEG_SEQUENCE_HEADER` blob of the transform's current output type, or empty.
pub(crate) fn sequence_header(t: &IMFTransform) -> Vec<u8> {
    // SAFETY: GetOutputCurrentType takes no pointers from us.
    let Ok(ty) = (unsafe { t.GetOutputCurrentType(0) }) else {
        return Vec::new();
    };
    // SAFETY: the key is a static MF GUID; the call only reads it.
    let Ok(size) = (unsafe { ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; size as usize];
    let mut got = 0u32;
    // SAFETY: `buf` holds `size` bytes, the blob's exact size; `got` is a live local.
    if unsafe {
        ty.GetBlob(
            &MF_MT_MPEG_SEQUENCE_HEADER,
            &mut buf,
            Some(&mut got as *mut u32),
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    buf.truncate(got as usize);
    buf
}
