//! Encoder identities and probe reports, plus the native (Media Foundation)
//! probe that produces them.
//!
//! [`probe_native`] test-encodes every entry of [`CANDIDATES`] through the
//! native encoders and reports which ones work.

use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync Video.
    Qsv,
    /// CPU software encoder (libx264 / libx265).
    Software,
}

/// Video codec family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecKind {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
    /// AV1.
    Av1,
}

/// Result of probing one encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderInfo {
    /// Backend this encoder belongs to.
    pub backend: Backend,
    /// Codec this encoder produces.
    pub codec: CodecKind,
    /// Encoder id, for example `h264_nvenc`.
    #[serde(alias = "ffmpeg_name")]
    pub encoder_name: String,
    /// True when the test encode exited successfully.
    pub works: bool,
    /// Failure reason (stderr tail, timeout, or not built in). None when `works`.
    pub error: Option<String>,
    /// Wall-clock time of the test encode in milliseconds.
    pub probe_ms: u64,
}

/// Full probe result for one probe run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// Build tag of the probing backend (see [`NATIVE_BUILD`]).
    pub build: String,
    /// One entry per known candidate, in candidate-table order.
    pub encoders: Vec<EncoderInfo>,
}

/// Known candidates: backend, codec, encoder id.
/// AV1 has no software entry: AV1 never selects software.
pub const CANDIDATES: &[(Backend, CodecKind, &str)] = &[
    (Backend::Nvenc, CodecKind::H264, "h264_nvenc"),
    (Backend::Nvenc, CodecKind::H265, "hevc_nvenc"),
    (Backend::Nvenc, CodecKind::Av1, "av1_nvenc"),
    (Backend::Amf, CodecKind::H264, "h264_amf"),
    (Backend::Amf, CodecKind::H265, "hevc_amf"),
    (Backend::Amf, CodecKind::Av1, "av1_amf"),
    (Backend::Qsv, CodecKind::H264, "h264_qsv"),
    (Backend::Qsv, CodecKind::H265, "hevc_qsv"),
    (Backend::Qsv, CodecKind::Av1, "av1_qsv"),
    (Backend::Software, CodecKind::H264, "libx264"),
    (Backend::Software, CodecKind::H265, "libx265"),
];

/// Build tag of the native (Media Foundation) probe report.
pub const NATIVE_BUILD: &str = "media-foundation";

/// Test-encode frame size for the native probe (16:9, even).
#[cfg(windows)]
const NATIVE_PROBE_WIDTH: u32 = 256;
/// Test-encode frame height for the native probe.
#[cfg(windows)]
const NATIVE_PROBE_HEIGHT: u32 = 144;
/// Frames submitted by one native test encode before flushing.
#[cfg(windows)]
const NATIVE_PROBE_FRAMES: i64 = 5;
/// Frame duration in 100 ns units (30 fps).
#[cfg(windows)]
const NATIVE_PROBE_FRAME_TICKS: i64 = 333_333;

/// Entries the native probe rejects without trying any encoder.
#[cfg(windows)]
fn native_unsupported(backend: Backend, codec: CodecKind) -> Option<&'static str> {
    match (backend, codec) {
        (_, CodecKind::Av1) => Some("AV1 is not supported by the native encoder yet"),
        (Backend::Software, CodecKind::H265) => Some("no software HEVC encoder in Windows"),
        _ => None,
    }
}

/// Probes every candidate of [`CANDIDATES`] with the native Media Foundation
/// encoders. Never panics: a panicking entry is recorded as `works: false`.
///
/// Hardware entries need a D3D11 device, created once per call. Software
/// entries (Microsoft MFTs with `hardware == false`) take system-memory NV12.
#[cfg(windows)]
pub fn probe_native() -> ProbeReport {
    let device = clipcheck_gpuconv::d3d::create_device(None)
        .map(|(device, _context)| device)
        .map_err(|e| e.to_string());

    let mut encoders = Vec::with_capacity(CANDIDATES.len());
    for &(backend, codec, name) in CANDIDATES {
        let (works, error, probe_ms) = if let Some(reason) = native_unsupported(backend, codec) {
            (false, Some(reason.to_string()), 0)
        } else {
            let started = Instant::now();
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                native_candidate(backend, codec, &device)
            }));
            let probe_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            match outcome {
                Ok(Ok(())) => (true, None, probe_ms),
                Ok(Err(reason)) => (false, Some(reason), probe_ms),
                Err(_) => (false, Some("probe panicked".to_string()), probe_ms),
            }
        };
        encoders.push(EncoderInfo {
            backend,
            codec,
            encoder_name: name.to_string(),
            works,
            error,
            probe_ms,
        });
    }

    ProbeReport {
        build: NATIVE_BUILD.to_string(),
        encoders,
    }
}

/// Runs the test encodes for one candidate: tries each matching native
/// descriptor in enumeration order and succeeds on the first that works.
#[cfg(windows)]
fn native_candidate(
    backend: Backend,
    codec: CodecKind,
    device: &Result<windows::Win32::Graphics::Direct3D11::ID3D11Device, String>,
) -> Result<(), String> {
    let (hw_codec, label) = match codec {
        CodecKind::H264 => (clipcheck_hwenc::Codec::H264, "H.264"),
        CodecKind::H265 => (clipcheck_hwenc::Codec::Hevc, "HEVC"),
        CodecKind::Av1 => return Err("AV1 is not supported by the native encoder yet".to_string()),
    };
    let vendor = match backend {
        Backend::Nvenc => Some((clipcheck_hwenc::Vendor::Nvidia, "NVIDIA")),
        Backend::Amf => Some((clipcheck_hwenc::Vendor::Amd, "AMD")),
        Backend::Qsv => Some((clipcheck_hwenc::Vendor::Intel, "Intel")),
        Backend::Software => None,
    };

    let listed = clipcheck_hwenc::enumerate(hw_codec).map_err(|e| e.to_string())?;
    let matching: Vec<clipcheck_hwenc::EncoderDescriptor> = listed
        .into_iter()
        .filter(|desc| match vendor {
            Some((wanted, _)) => desc.hardware && desc.vendor == wanted,
            None => !desc.hardware,
        })
        .collect();

    let vendor_name = vendor.map_or("software", |(_, name)| name);
    let mut last_err = format!("no {vendor_name} {label} encoder found");
    for desc in &matching {
        match native_test_encode(desc, hw_codec, device) {
            Ok(()) => return Ok(()),
            Err(reason) => last_err = reason,
        }
    }
    Err(last_err)
}

/// Opens `desc` and encodes a few frames, succeeding when at least one
/// packet comes out (including after flush).
#[cfg(windows)]
fn native_test_encode(
    desc: &clipcheck_hwenc::EncoderDescriptor,
    codec: clipcheck_hwenc::Codec,
    device: &Result<windows::Win32::Graphics::Direct3D11::ID3D11Device, String>,
) -> Result<(), String> {
    let cfg = clipcheck_hwenc::VideoEncoderConfig {
        codec,
        width: NATIVE_PROBE_WIDTH,
        height: NATIVE_PROBE_HEIGHT,
        fps: 30,
        rate: clipcheck_hwenc::RateControl::Cbr { kbps: 1000 },
        gop_frames: 30,
        quality_vs_speed: 50,
        low_latency: true,
    };
    let to_str = |e: clipcheck_hwenc::EncError| e.to_string();

    let mut packets = 0usize;
    if desc.hardware {
        let device = device.as_ref().map_err(Clone::clone)?;
        let converter = clipcheck_gpuconv::d3d::GpuConverter::new(
            device,
            NATIVE_PROBE_WIDTH,
            NATIVE_PROBE_HEIGHT,
        )
        .map_err(|e| e.to_string())?;
        let texture = converter.create_nv12_texture().map_err(|e| e.to_string())?;
        let mut encoder = clipcheck_hwenc::open(desc, Some(device), &cfg).map_err(to_str)?;
        for i in 0..NATIVE_PROBE_FRAMES {
            let frame = clipcheck_hwenc::InputFrame::Texture(&texture);
            let out = encoder
                .encode(frame, i * NATIVE_PROBE_FRAME_TICKS, i == 0)
                .map_err(to_str)?;
            packets += out.len();
        }
        packets += encoder.flush().map_err(to_str)?.len();
    } else {
        let luma = (NATIVE_PROBE_WIDTH * NATIVE_PROBE_HEIGHT) as usize;
        let mut nv12 = vec![16u8; luma];
        nv12.extend(vec![128u8; luma / 2]);
        let mut encoder = clipcheck_hwenc::open(desc, None, &cfg).map_err(to_str)?;
        for i in 0..NATIVE_PROBE_FRAMES {
            let frame = clipcheck_hwenc::InputFrame::Nv12 {
                data: &nv12,
                width: NATIVE_PROBE_WIDTH,
                height: NATIVE_PROBE_HEIGHT,
            };
            let out = encoder
                .encode(frame, i * NATIVE_PROBE_FRAME_TICKS, i == 0)
                .map_err(to_str)?;
            packets += out.len();
        }
        packets += encoder.flush().map_err(to_str)?.len();
    }

    if packets >= 1 {
        Ok(())
    } else {
        Err(format!("{} produced no packets", desc.name))
    }
}

#[cfg(test)]
mod encoder_info_serde_tests {
    use super::*;

    #[test]
    fn encoder_name_reads_legacy_key_and_writes_new_key() {
        let legacy = r#"{"backend":"nvenc","codec":"h264","ffmpeg_name":"h264_nvenc","works":true,"error":null,"probe_ms":5}"#;
        let info: EncoderInfo = serde_json::from_str(legacy).expect("legacy key deserializes");
        assert_eq!(info.encoder_name, "h264_nvenc");

        let json = serde_json::to_string(&info).expect("serializes");
        assert!(json.contains("\"encoder_name\""), "json: {json}");
        assert!(!json.contains("ffmpeg_name"), "json: {json}");
    }
}

#[cfg(all(test, windows))]
mod native_probe_tests {
    use super::*;

    #[test]
    fn probe_native_smoke() {
        let report = probe_native();
        for e in &report.encoders {
            eprintln!(
                "{:<12} works={:<5} ms={:<6} error={:?}",
                e.encoder_name, e.works, e.probe_ms, e.error
            );
        }
        assert_eq!(report.encoders.len(), CANDIDATES.len());
        assert_eq!(report.build, NATIVE_BUILD);
    }
}
