//! Oracle tests for the fragmented MP4 writer (`clipcheck_mp4::fmp4`).
//!
//! Input: a clipcheck-testkit MP4 (native Media Foundation H.264 + AAC), read back with
//! `clipcheck_mp4::reader`; its samples are re-muxed with `write_init` / `write_fragment`.
//! Judge: `clipcheck_mp4::reader` (track layout, sample counts, durations, codec config,
//! titles) and `clipcheck_mfdec` (video decodes cleanly, frame count).
//!
//! Dropped from the earlier external-tool suite (D62): `oracle_3_hevc_video_only` (needs HEVC encoded
//! input; clipcheck-testkit generates H.264 only).
//!
//! Skips (with an eprintln) when the native test media generator is unavailable.

use clipcheck_mp4::aac::aac_lc_asc;
use clipcheck_mp4::avc::avc_config;
use clipcheck_mp4::fmp4::{
    write_fragment, write_init, AudioTrackSpec, InitSpec, Sample, TrackRun, VideoConfig,
    VideoTrackSpec,
};
use clipcheck_mp4::reader::{read_movie, Track, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use clipcheck_testkit::{AudioSpec, MediaSpec, VideoSpec};
use std::fs;
use std::path::{Path, PathBuf};

const VIDEO_FRAMES: usize = 60;
const GOP: usize = 30;
const VIDEO_TS: u32 = 90_000;
const VIDEO_DUR: u32 = 3_000; // 90k / 30 fps
const AAC_RATE: u32 = 48_000;
const AAC_FRAME: u64 = 1_024;

// ---------------------------------------------------------------------------
// Source media (clipcheck-testkit, read back with clipcheck_mp4::reader)
// ---------------------------------------------------------------------------

/// One video access unit in both forms the writers need.
struct Au {
    /// Annex B; SPS/PPS prepended to keyframes that lack them (for `avc_config`).
    annexb: Vec<u8>,
    /// Length-prefixed sample bytes exactly as stored in the source MP4.
    lp: Vec<u8>,
    key: bool,
}

struct Source {
    aus: Vec<Au>,
    /// Raw AAC access units (no ADTS header).
    aac: Vec<Vec<u8>>,
}

/// 2 s 640x360 30 fps H.264 (GOP 30) plus one 48 kHz stereo AAC sine track.
fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.mp4");
    let spec = MediaSpec {
        secs: 2.0,
        video: Some(VideoSpec {
            width: 640,
            height: 360,
            fps: 30,
            gop_frames: GOP as u32,
        }),
        audio: vec![AudioSpec::sine(440.0)],
    };
    clipcheck_testkit::write_mp4(&path, &spec).expect("testkit write_mp4");
    let movie = read_movie(&path).expect("read source mp4");
    let bytes = fs::read(&path).unwrap();
    let v = movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .expect("source video track");
    let a = movie
        .tracks
        .iter()
        .find(|t| t.is_audio())
        .expect("source audio track");

    let param_sets = avcc_param_sets(&v.sample_entries[0]);
    let aus = v
        .samples
        .iter()
        .map(|s| {
            let lp = sample_bytes(&bytes, s.offset, s.size).to_vec();
            let mut annexb = Vec::new();
            if s.keyframe && !first_nal_is(&lp, 7) {
                annexb.extend_from_slice(&param_sets);
            }
            annexb.extend_from_slice(&avcc_to_annexb(&lp));
            Au {
                annexb,
                lp,
                key: s.keyframe,
            }
        })
        .collect();
    let aac = a
        .samples
        .iter()
        .map(|s| sample_bytes(&bytes, s.offset, s.size).to_vec())
        .collect();
    Source { aus, aac }
}

fn sample_bytes(file: &[u8], offset: u64, size: u32) -> &[u8] {
    let start = usize::try_from(offset).unwrap();
    &file[start..start + size as usize]
}

/// True when the first length-prefixed NAL of an AVCC sample has type `ty`.
fn first_nal_is(data: &[u8], ty: u8) -> bool {
    data.len() > 4 && data[4] & 0x1f == ty
}

/// Converts 4-byte length-prefixed NALs to Annex B start codes.
fn avcc_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let n = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        assert!(i + n <= data.len(), "AVCC NAL overruns sample");
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out
}

/// SPS and PPS from the `avcC` box of an `avc1`/`avc3` sample entry, as Annex B.
fn avcc_param_sets(entry: &[u8]) -> Vec<u8> {
    let pos = entry
        .windows(4)
        .position(|w| w == b"avcC")
        .expect("avcC box in sample entry");
    let d = &entry[pos + 4..];
    let mut out = Vec::new();
    let mut p = 5; // configurationVersion .. lengthSizeMinusOne, then numSPS
    let nsps = (d[p] & 0x1f) as usize;
    p += 1;
    for _ in 0..nsps {
        let n = u16::from_be_bytes([d[p], d[p + 1]]) as usize;
        p += 2;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&d[p..p + n]);
        p += n;
    }
    let npps = d[p] as usize;
    p += 1;
    for _ in 0..npps {
        let n = u16::from_be_bytes([d[p], d[p + 1]]) as usize;
        p += 2;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&d[p..p + n]);
        p += n;
    }
    out
}

// ---------------------------------------------------------------------------
// Judges
// ---------------------------------------------------------------------------

/// Decodes all video frames of `file` with Media Foundation; returns the frame count.
#[cfg(windows)]
fn decode_frames(file: &Path) -> Result<usize, String> {
    use clipcheck_mfdec::{OutFormat, VideoDecoder};
    let mut dec = VideoDecoder::open(file, OutFormat::Nv12).map_err(|e| e.to_string())?;
    let mut n = 0;
    while dec.next_frame().map_err(|e| e.to_string())?.is_some() {
        n += 1;
    }
    Ok(n)
}

#[cfg(not(windows))]
fn decode_frames(_file: &Path) -> Result<usize, String> {
    Err("mfdec needs Windows".to_string())
}

fn assert_decodes_clean(file: &Path, frames: usize) {
    let n = decode_frames(file).unwrap_or_else(|e| panic!("decode {}: {e}", file.display()));
    assert_eq!(n, frames, "decoded frames of {}", file.display());
}

fn check_video(t: &Track) {
    match &t.kind {
        TrackKind::Video {
            codec,
            width,
            height,
        } => {
            assert_eq!(codec, "h264", "video codec");
            assert_eq!((*width, *height), (640, 360), "video size");
        }
        other => panic!("expected video track, got {other:?}"),
    }
    assert_eq!(t.samples.len(), VIDEO_FRAMES, "video frames");
    assert!(
        (t.duration_secs() - 2.0).abs() <= 0.05,
        "video duration {}",
        t.duration_secs()
    );
}

fn check_aac(t: &Track, title: &str) {
    match &t.kind {
        TrackKind::Audio {
            codec,
            channels,
            sample_rate,
        } => {
            assert_eq!(codec, "aac", "audio codec");
            assert_eq!(*sample_rate, AAC_RATE, "sample rate");
            assert_eq!(*channels, 2, "channels");
        }
        other => panic!("expected audio track, got {other:?}"),
    }
    // The writer stores the title in trak/udta/name; the reader exposes it as `title`.
    assert_eq!(t.title.as_deref(), Some(title), "audio title");
    assert!(
        (t.duration_secs() - 2.0).abs() <= 0.05,
        "audio {title} duration {}",
        t.duration_secs()
    );
}

// ---------------------------------------------------------------------------
// Fragment building
// ---------------------------------------------------------------------------

fn video_config(aus: &[Au]) -> VideoConfig {
    VideoConfig::Avc(avc_config(&aus[0].annexb).expect("avc_config returned None for first AU"))
}

/// Splits video samples at keyframes; returns (base_decode_time, samples) per GOP.
fn video_gops(aus: &[Au]) -> Vec<(u64, Vec<Sample>)> {
    let mut gops: Vec<(u64, Vec<Sample>)> = Vec::new();
    let mut t: u64 = 0;
    for au in aus {
        if au.key || gops.is_empty() {
            gops.push((t, Vec::new()));
        }
        gops.last_mut().unwrap().1.push(Sample {
            data: au.lp.clone(),
            duration: VIDEO_DUR,
            cts_offset: 0,
            keyframe: au.key,
        });
        t += VIDEO_DUR as u64;
    }
    gops
}

fn audio_samples(frames: &[Vec<u8>], range: std::ops::Range<usize>) -> Vec<Sample> {
    frames[range]
        .iter()
        .map(|f| Sample {
            data: f.clone(),
            duration: AAC_FRAME as u32,
            cts_offset: 0,
            keyframe: true,
        })
        .collect()
}

/// Builds a complete file: init segment followed by one fragment per GOP.
/// Audio tracks (one per title, identical frames) are split by time against the video GOPs.
fn build_mp4(aus: &[Au], audio_titles: &[&str], aac: &[Vec<u8>]) -> Vec<u8> {
    let asc = aac_lc_asc(AAC_RATE, 2).expect("aac_lc_asc(48000, 2) returned None");
    let init = write_init(&InitSpec {
        video: VideoTrackSpec {
            config: video_config(aus),
            width: 640,
            height: 360,
            timescale: VIDEO_TS,
        },
        audio: audio_titles
            .iter()
            .map(|title| AudioTrackSpec {
                sample_rate: AAC_RATE,
                channels: 2,
                asc: asc.clone(),
                bitrate: 128_000,
                title: title.to_string(),
            })
            .collect(),
    });

    let gops = video_gops(aus);
    let n_gops = gops.len();
    let mut out = init;
    let mut audio_pos = 0usize;
    for (k, (base, samples)) in gops.into_iter().enumerate() {
        let end_ticks = base + samples.len() as u64 * VIDEO_DUR as u64;
        let end_s = end_ticks as f64 / VIDEO_TS as f64;
        // Audio frames whose start time is before this fragment's end go here.
        let mut audio_end = audio_pos;
        if k + 1 == n_gops {
            audio_end = aac.len();
        } else {
            while audio_end < aac.len()
                && (audio_end as f64) * (AAC_FRAME as f64) / (AAC_RATE as f64) < end_s
            {
                audio_end += 1;
            }
        }

        let mut runs = vec![TrackRun {
            track_id: 1,
            base_decode_time: base,
            samples,
        }];
        if audio_end > audio_pos {
            for i in 0..audio_titles.len() {
                runs.push(TrackRun {
                    track_id: i as u32 + 2,
                    base_decode_time: audio_pos as u64 * AAC_FRAME,
                    samples: audio_samples(aac, audio_pos..audio_end),
                });
            }
        }
        audio_pos = audio_end;
        out.extend_from_slice(&write_fragment(k as u32 + 1, &runs));
    }
    out
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn oracle_1_video_only_h264() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    assert_eq!(
        src.aus.len(),
        VIDEO_FRAMES,
        "expected {VIDEO_FRAMES} access units"
    );
    assert!(
        src.aus[0].key && src.aus[GOP].key,
        "GOP keyframes not detected"
    );

    let dir = tempfile::tempdir().unwrap();
    let file = write_file(dir.path(), "video_only.mp4", &build_mp4(&src.aus, &[], &[]));
    let mv = read_movie(&file).expect("read output");
    assert_eq!(mv.tracks.len(), 1, "expected 1 track");
    check_video(&mv.tracks[0]);
    assert_decodes_clean(&file, VIDEO_FRAMES);
}

#[test]
fn oracle_2_video_plus_two_audio_tracks() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    assert!(
        src.aac.len() >= 90,
        "expected ~94 AAC frames, got {}",
        src.aac.len()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = write_file(
        dir.path(),
        "av.mp4",
        &build_mp4(&src.aus, &["Game", "Mic"], &src.aac),
    );
    let mv = read_movie(&file).expect("read output");
    assert_eq!(mv.tracks.len(), 3, "expected 3 tracks");
    check_video(
        mv.tracks
            .iter()
            .find(|t| t.is_video())
            .expect("video track"),
    );
    let audio: Vec<&Track> = mv.tracks.iter().filter(|t| t.is_audio()).collect();
    assert_eq!(audio.len(), 2, "expected 2 audio tracks");
    for (t, title) in audio.iter().zip(["Game", "Mic"]) {
        check_aac(t, title);
    }
    assert_decodes_clean(&file, VIDEO_FRAMES);
}

#[test]
fn oracle_4_concat_remux_of_fragmented_files() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    let dir = tempfile::tempdir().unwrap();
    let bytes = build_mp4(&src.aus, &[], &[]);
    let a = write_file(dir.path(), "a.mp4", &bytes);
    let b = write_file(dir.path(), "b.mp4", &bytes);

    // The app's own remux is the subject here; the reader and mfdec judge its output.
    let out = dir.path().join("out.mp4");
    concat_remux(&[a, b], &out, &RemuxOptions::default()).expect("concat_remux");

    let mv = read_movie(&out).expect("read concat output");
    let video: Vec<&Track> = mv.tracks.iter().filter(|t| t.is_video()).collect();
    assert_eq!(video.len(), 1, "expected 1 video track");
    assert_eq!(
        video[0].samples.len(),
        2 * VIDEO_FRAMES,
        "concat video frames"
    );
    assert!(
        (video[0].duration_secs() - 4.0).abs() <= 0.05,
        "concat video duration {}",
        video[0].duration_secs()
    );
    assert_decodes_clean(&out, 2 * VIDEO_FRAMES);
}

#[test]
fn oracle_5_write_fragment_edge_cases() {
    // Pure structural checks; no media needed.
    assert!(
        write_fragment(1, &[]).is_empty(),
        "no runs must yield empty output"
    );

    let empty_only = [TrackRun {
        track_id: 1,
        base_decode_time: 0,
        samples: Vec::new(),
    }];
    assert!(
        write_fragment(1, &empty_only).is_empty(),
        "a single empty run must yield empty output"
    );

    let payload: Vec<u8> = vec![0x00, 0x00, 0x00, 0x02, 0x65, 0x88];
    let mixed = [
        TrackRun {
            track_id: 2,
            base_decode_time: 0,
            samples: Vec::new(),
        },
        TrackRun {
            track_id: 1,
            base_decode_time: 0,
            samples: vec![Sample {
                data: payload.clone(),
                duration: VIDEO_DUR,
                cts_offset: 0,
                keyframe: true,
            }],
        },
    ];
    let out = write_fragment(7, &mixed);
    assert!(
        !out.is_empty(),
        "empty run + non-empty run must yield a fragment"
    );
    assert_eq!(&out[4..8], b"moof", "fragment must start with a moof box");
    assert!(
        out.ends_with(&payload),
        "sample payload must end the mdat box"
    );
}
