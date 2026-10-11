//! Windows implementation: software H.264 + AAC -> `SegmentMuxer`.

use std::path::{Path, PathBuf};

use clipcheck_aacenc::{AacEncoder, AacPacket};
use clipcheck_hwenc::{
    Codec, EncodedPacket, InputFrame, RateControl, VideoEncoder, VideoEncoderConfig,
};
use clipcheck_mp4::fmp4::AudioTrackSpec;
use clipcheck_mp4::segment::{AudioPacket, SegmentConfig, SegmentMuxer, VideoPacket};
use clipcheck_mp4::VideoCodec;

use crate::{AudioSpec, MediaSpec, TestkitError, VideoSpec};

const HNS: i64 = 10_000_000;
const AAC_KBPS: u32 = 128;

fn be<E: std::fmt::Display>(ctx: &'static str) -> impl Fn(E) -> TestkitError {
    move |e| TestkitError::Backend(format!("{ctx}: {e}"))
}

fn open_video(v: &VideoSpec) -> Result<Box<dyn VideoEncoder>, TestkitError> {
    if v.width == 0 || v.height == 0 || !v.width.is_multiple_of(2) || !v.height.is_multiple_of(2) {
        return Err(TestkitError::BadSpec(
            "video size must be even and > 0".into(),
        ));
    }
    if v.fps == 0 || v.gop_frames == 0 {
        return Err(TestkitError::BadSpec(
            "fps and gop_frames must be > 0".into(),
        ));
    }
    let kbps = (u64::from(v.width) * u64::from(v.height) * u64::from(v.fps) / 20_000).max(300);
    let cfg = VideoEncoderConfig {
        codec: Codec::H264,
        width: v.width,
        height: v.height,
        fps: v.fps,
        rate: RateControl::Cbr { kbps: kbps as u32 },
        gop_frames: v.gop_frames,
        quality_vs_speed: 50,
        low_latency: false,
    };
    let list = clipcheck_hwenc::enumerate(Codec::H264).map_err(be("enumerate encoders"))?;
    let mut last = String::from("no software H.264 encoder");
    for desc in list.iter().filter(|d| !d.hardware) {
        match clipcheck_hwenc::open(desc, None, &cfg) {
            Ok(e) => return Ok(e),
            Err(e) => last = format!("{}: {e}", desc.name),
        }
    }
    Err(TestkitError::Backend(format!(
        "open software encoder: {last}"
    )))
}

pub fn available() -> bool {
    let v = VideoSpec {
        width: 128,
        height: 96,
        fps: 30,
        gop_frames: 30,
    };
    clipcheck_hwenc::mf_startup().is_ok()
        && open_video(&v).is_ok()
        && AacEncoder::new(48_000, 2, AAC_KBPS).is_ok()
}

/// Deterministic NV12 frame: gradient background, a bright bar moving 8 px per frame, chroma
/// that drifts with the frame index.
fn nv12_frame(w: u32, h: u32, i: u64, buf: &mut Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    buf.clear();
    buf.resize(w * h * 3 / 2, 0);
    let bar = (i as usize * 8) % w;
    let bar_w = (w / 10).max(4);
    for y in 0..h {
        for x in 0..w {
            let in_bar = (x + w - bar) % w < bar_w;
            buf[y * w + x] = if in_bar {
                235
            } else {
                (32 + (x * 96 / w) + (y * 64 / h)) as u8
            };
        }
    }
    let uv = &mut buf[w * h..];
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            uv[y * w + x * 2] = (64 + (x * 128 / (w / 2).max(1)) + (i as usize * 3) % 64) as u8;
            uv[y * w + x * 2 + 1] = (192 - (y * 128 / (h / 2).max(1))) as u8;
        }
    }
}

/// Muxes video packets; returns true when the muxer has seen at least one.
fn mux_video(muxer: &mut SegmentMuxer, packets: Vec<EncodedPacket>) -> Result<bool, TestkitError> {
    let any = !packets.is_empty();
    for p in packets {
        muxer
            .push_video(VideoPacket {
                data: p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .map_err(be("mux video"))?;
    }
    Ok(any)
}

pub fn write_segments(
    dir: &Path,
    spec: &MediaSpec,
    segment_secs: f64,
) -> Result<Vec<PathBuf>, TestkitError> {
    let video = spec
        .video
        .as_ref()
        .ok_or_else(|| TestkitError::BadSpec("a video track is required".into()))?;
    if spec.secs.is_nan() || spec.secs <= 0.0 || segment_secs.is_nan() || segment_secs <= 0.0 {
        return Err(TestkitError::BadSpec(
            "secs and segment_secs must be > 0".into(),
        ));
    }
    clipcheck_hwenc::mf_startup().map_err(be("MFStartup"))?;
    let mut enc = open_video(video)?;

    let mut aac = Vec::new();
    let mut tracks = Vec::new();
    for a in &spec.audio {
        let e = AacEncoder::new(a.sample_rate, a.channels, AAC_KBPS).map_err(be("AAC encoder"))?;
        tracks.push(AudioTrackSpec {
            sample_rate: a.sample_rate,
            channels: a.channels,
            asc: e.asc().to_vec(),
            bitrate: AAC_KBPS * 1000,
            title: a.title.clone().unwrap_or_default(),
        });
        aac.push(e);
    }
    let mut muxer = SegmentMuxer::new(SegmentConfig {
        out_dir: dir.to_path_buf(),
        segment_secs,
        fps: video.fps,
        codec: VideoCodec::H264,
        width: video.width,
        height: video.height,
        audio: tracks,
    })
    .map_err(be("segment muxer"))?;

    let frames = (spec.secs * f64::from(video.fps)).round().max(1.0) as u64;
    let mut written = vec![0u64; aac.len()];
    let mut nv12 = Vec::new();
    // The encoder emits its first packet a few frames late and the muxer drops audio that
    // arrives before its first video packet, so audio is held back until video has started.
    let mut started = false;
    let mut pending: Vec<(usize, AacPacket)> = Vec::new();
    for i in 0..frames {
        let pts = (i as i128 * i128::from(HNS) / i128::from(video.fps)) as i64;
        nv12_frame(video.width, video.height, i, &mut nv12);
        let packets = enc
            .encode(
                InputFrame::Nv12 {
                    data: &nv12,
                    width: video.width,
                    height: video.height,
                },
                pts,
                (i).is_multiple_of(u64::from(video.gop_frames)),
            )
            .map_err(be("encode video"))?;
        started |= mux_video(&mut muxer, packets)?;
        // Audio up to the end of this frame (and to the exact end on the last one).
        for (t, a) in spec.audio.iter().enumerate() {
            let target = if i + 1 == frames {
                (spec.secs * f64::from(a.sample_rate)).round() as u64
            } else {
                (i + 1) * u64::from(a.sample_rate) / u64::from(video.fps)
            };
            pending.extend(
                make_audio(&mut aac[t], a, &mut written[t], target)?
                    .into_iter()
                    .map(|p| (t, p)),
            );
        }
        if started {
            for (t, p) in pending.drain(..) {
                push_packet(&mut muxer, t, p)?;
            }
        }
    }
    mux_video(&mut muxer, enc.flush().map_err(be("flush video"))?)?;
    for (t, p) in pending.drain(..) {
        push_packet(&mut muxer, t, p)?;
    }
    for (t, e) in aac.iter_mut().enumerate() {
        for p in e.flush().map_err(be("flush AAC"))? {
            push_packet(&mut muxer, t, p)?;
        }
    }
    muxer.finish().map_err(be("finish muxer"))
}

fn push_packet(muxer: &mut SegmentMuxer, track: usize, p: AacPacket) -> Result<(), TestkitError> {
    muxer
        .push_audio(
            track,
            AudioPacket {
                data: p.data,
                pts: p.pts,
                samples: p.samples,
            },
        )
        .map_err(be("mux audio"))
}

fn make_audio(
    enc: &mut AacEncoder,
    a: &AudioSpec,
    written: &mut u64,
    target: u64,
) -> Result<Vec<AacPacket>, TestkitError> {
    if target <= *written {
        return Ok(Vec::new());
    }
    let ch = usize::from(a.channels);
    let mut buf = Vec::with_capacity((target - *written) as usize * ch);
    let w = 2.0 * std::f64::consts::PI * f64::from(a.freq_hz) / f64::from(a.sample_rate);
    for n in *written..target {
        let s = ((n as f64 * w).sin() * 0.5) as f32;
        for _ in 0..ch {
            buf.push(s);
        }
    }
    let pts = (*written as i128 * i128::from(HNS) / i128::from(a.sample_rate)) as i64;
    *written = target;
    enc.push(&buf, pts).map_err(be("AAC encode"))
}
