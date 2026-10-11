//! Oracle tests for `clipcheck_mp4::segment`.
//!
//! Input: a clipcheck-testkit MP4 (native Media Foundation H.264 + AAC), read back with
//! `clipcheck_mp4::reader`; its samples are fed to `SegmentMuxer` as Annex B / raw AAC.
//! Judge: `clipcheck_mp4::reader` (track layout, sample counts, durations, keyframes, titles,
//! codec config) and `clipcheck_mfdec` (video decodes cleanly, frame count).
//!
//! Skips (with an eprintln) when the native test media generator is unavailable.

use clipcheck_mp4::aac::aac_lc_asc;
use clipcheck_mp4::annexb::VideoCodec;
use clipcheck_mp4::fmp4::AudioTrackSpec;
use clipcheck_mp4::reader::{read_movie, Track, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use clipcheck_mp4::segment::{AudioPacket, SegmentConfig, SegmentMuxer, VideoPacket};
use clipcheck_testkit::{AudioSpec, MediaSpec, VideoSpec};
use std::fs;
use std::path::Path;

/// Source media read back from a testkit MP4.
struct Source {
    /// Annex B access units; SPS/PPS (from avcC) prepended to keyframes that lack them.
    aus: Vec<Vec<u8>>,
    /// Keyframe flag per video sample.
    keyframes: Vec<bool>,
    /// Presentation time of each video sample, 100 ns.
    video_pts: Vec<i64>,
    /// Raw AAC access units (no ADTS header).
    aac: Vec<Vec<u8>>,
    /// Presentation time of each AAC frame, 100 ns.
    aac_pts: Vec<i64>,
}

/// Source: 6 s 1280x720 30 fps H.264, GOP 60, one 48 kHz stereo AAC track.
fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.mp4");
    let spec = MediaSpec {
        secs: 6.0,
        video: Some(VideoSpec {
            width: 1280,
            height: 720,
            fps: 30,
            gop_frames: 60,
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
    let mut src = Source {
        aus: Vec::new(),
        keyframes: Vec::new(),
        video_pts: Vec::new(),
        aac: Vec::new(),
        aac_pts: Vec::new(),
    };
    for s in &v.samples {
        let data = sample_bytes(&bytes, s.offset, s.size);
        let mut au = Vec::new();
        if s.keyframe && !first_nal_is(data, 7) {
            au.extend_from_slice(&param_sets);
        }
        au.extend_from_slice(&avcc_to_annexb(data));
        src.aus.push(au);
        src.keyframes.push(s.keyframe);
        src.video_pts
            .push(to_100ns(s.dts + i64::from(s.cts_offset), v.timescale));
    }
    for s in &a.samples {
        src.aac
            .push(sample_bytes(&bytes, s.offset, s.size).to_vec());
        src.aac_pts.push(to_100ns(s.dts, a.timescale));
    }
    src
}

fn sample_bytes(file: &[u8], offset: u64, size: u32) -> &[u8] {
    let start = usize::try_from(offset).unwrap();
    &file[start..start + size as usize]
}

fn to_100ns(t: i64, timescale: u32) -> i64 {
    (i128::from(t) * 10_000_000 / i128::from(timescale)) as i64
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

fn assert_video(t: &Track, w: u32, h: u32, frames: usize, ctx: &str) {
    match &t.kind {
        TrackKind::Video {
            codec,
            width,
            height,
        } => {
            assert_eq!(codec, "h264", "{ctx} video codec");
            assert_eq!((*width, *height), (w, h), "{ctx} video size");
        }
        other => panic!("{ctx}: expected video track, got {other:?}"),
    }
    assert_eq!(t.samples.len(), frames, "{ctx} video frames");
}

fn assert_aac(t: &Track, ctx: &str) {
    match &t.kind {
        TrackKind::Audio {
            codec,
            channels,
            sample_rate,
        } => {
            assert_eq!(codec, "aac", "{ctx} audio codec");
            assert_eq!(*sample_rate, 48_000, "{ctx} sample rate");
            assert_eq!(*channels, 2, "{ctx} channels");
        }
        other => panic!("{ctx}: expected audio track, got {other:?}"),
    }
}

/// Presentation time of the first sample of `t`, seconds.
fn first_pts(t: &Track) -> f64 {
    let s = t.samples[0];
    (s.dts + i64::from(s.cts_offset)) as f64 / f64::from(t.timescale)
}

// ---------------------------------------------------------------------------

fn track(title: &str) -> AudioTrackSpec {
    AudioTrackSpec {
        sample_rate: 48_000,
        channels: 2,
        asc: aac_lc_asc(48_000, 2).expect("asc"),
        bitrate: 160_000,
        title: title.to_string(),
    }
}

fn cfg(dir: &Path, secs: f64, codec: VideoCodec, audio: Vec<AudioTrackSpec>) -> SegmentConfig {
    SegmentConfig {
        out_dir: dir.to_path_buf(),
        segment_secs: secs,
        fps: 30,
        codec,
        width: 1280,
        height: 720,
        audio,
    }
}

/// Pushes video AUs from `first_au` and all AAC frames, merged in pts order (video before
/// audio on ties); each audio frame goes to the first `tracks` audio tracks.
fn feed(m: &mut SegmentMuxer, src: &Source, first_au: usize, tracks: usize) {
    // (pts, kind: 0 = video / 1 = audio, index)
    let mut ev: Vec<(i64, u8, usize)> = Vec::new();
    for i in first_au..src.aus.len() {
        ev.push((src.video_pts[i], 0, i));
    }
    for k in 0..src.aac.len() {
        ev.push((src.aac_pts[k], 1, k));
    }
    ev.sort_by_key(|e| (e.0, e.1));
    for (pts, kind, idx) in ev {
        if kind == 0 {
            let _ = m.push_video(VideoPacket {
                data: src.aus[idx].clone(),
                pts,
                dts: pts,
                keyframe: src.keyframes[idx],
            });
        } else {
            for t in 0..tracks {
                let _ = m.push_audio(
                    t,
                    AudioPacket {
                        data: src.aac[idx].clone(),
                        pts,
                        samples: 1024,
                    },
                );
            }
        }
    }
}

#[test]
fn h264_two_audio_tracks_segments_match_oracle() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    assert_eq!(src.aus.len(), 180, "source video frames");
    assert!(src.aac.len() > 200, "AAC frame count {}", src.aac.len());

    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let mut m = SegmentMuxer::new(cfg(
        &out_dir,
        2.0,
        VideoCodec::H264,
        vec![track("Game"), track("Mic")],
    ))
    .expect("new");
    feed(&mut m, &src, 0, 2);
    let paths = m.finish().expect("finish");
    assert_eq!(paths.len(), 3, "segment count");

    // segments.csv
    let csv = fs::read_to_string(out_dir.join("segments.csv")).expect("segments.csv");
    let rows: Vec<Vec<String>> = csv
        .lines()
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect();
    assert_eq!(rows.len(), 3, "csv rows: {csv:?}");
    let mut prev_end: Option<f64> = None;
    for (k, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), 3, "row {k}: {row:?}");
        assert_eq!(row[0], format!("seg-{k:06}.mp4"), "csv name row {k}");
        assert_eq!(
            paths[k].file_name().unwrap().to_str().unwrap(),
            row[0],
            "returned path {k}"
        );
        assert_eq!(
            row[1].split('.').nth(1).map(str::len),
            Some(6),
            "start decimals {}",
            row[1]
        );
        assert_eq!(
            row[2].split('.').nth(1).map(str::len),
            Some(6),
            "end decimals {}",
            row[2]
        );
        let s: f64 = row[1].parse().unwrap();
        let e: f64 = row[2].parse().unwrap();
        assert!((s - 2.0 * k as f64).abs() <= 0.034, "start row {k} = {s}");
        assert!((e - s - 2.0).abs() <= 0.05, "duration row {k} = {}", e - s);
        if let Some(pe) = prev_end {
            assert!((pe - s).abs() < 1e-6, "end_{} {pe} != start_{k} {s}", k - 1);
        }
        prev_end = Some(e);
        println!("csv row {k}: start={s:.6} end={e:.6} dur={:.6}", e - s);
    }

    // Per-segment oracle checks.
    for (k, seg) in paths.iter().enumerate() {
        let ctx = format!("seg {k}");
        let m = read_movie(seg).expect("read segment");
        let video: Vec<&Track> = m.tracks.iter().filter(|t| t.is_video()).collect();
        let audio: Vec<&Track> = m.tracks.iter().filter(|t| t.is_audio()).collect();
        assert_eq!(video.len(), 1, "{ctx} video tracks");
        assert_eq!(audio.len(), 2, "{ctx} audio tracks");
        assert_video(video[0], 1280, 720, 60, &ctx);
        // The muxer writes the track title to trak/udta/name; the reader exposes it as `title`.
        let mut titles: Vec<String> = audio
            .iter()
            .map(|a| a.title.clone().unwrap_or_default())
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            vec!["Game".to_string(), "Mic".to_string()],
            "{ctx} audio titles"
        );
        for a in &audio {
            assert_aac(a, &ctx);
        }
        assert!(
            (m.duration_secs - 2.0).abs() <= 0.1,
            "{ctx} duration {}",
            m.duration_secs
        );
        let first = video[0];
        assert!(
            first.samples[0].keyframe,
            "{ctx} first video sample keyframe"
        );
        assert!(
            first_pts(first).abs() <= 0.034,
            "{ctx} first pts {}",
            first_pts(first)
        );
        assert_decodes_clean(seg, 60);
        println!(
            "seg {k}: duration={:.6} video_frames=60 first_pts={:.6}",
            m.duration_secs,
            first_pts(first)
        );
    }
}

#[test]
fn concat_of_segments_matches_app_remux() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let mut m = SegmentMuxer::new(cfg(
        &out_dir,
        2.0,
        VideoCodec::H264,
        vec![track("Game"), track("Mic")],
    ))
    .expect("new");
    feed(&mut m, &src, 0, 2);
    let paths = m.finish().expect("finish");
    assert_eq!(paths.len(), 3);

    // The app's own remux is the subject here; the reader and mfdec judge its output.
    let out = dir.path().join("out.mp4");
    concat_remux(&paths, &out, &RemuxOptions::default()).expect("concat_remux");

    let mv = read_movie(&out).expect("read concat output");
    assert_eq!(mv.tracks.len(), 3, "concat tracks");
    assert!(
        (mv.duration_secs - 6.0).abs() <= 0.1,
        "concat duration {}",
        mv.duration_secs
    );
    let video = mv.tracks.iter().find(|t| t.is_video()).expect("video");
    assert_video(video, 1280, 720, 180, "concat");
    let vdur = video.duration_secs();
    for a in mv.tracks.iter().filter(|t| t.is_audio()) {
        let adur = a.duration_secs();
        assert!(
            (adur - vdur).abs() <= 0.1,
            "A/V sync: audio {adur} vs video {vdur}"
        );
    }
    assert_decodes_clean(&out, 180);
    println!(
        "concat: duration={:.6} video_stream_duration={vdur:.6}",
        mv.duration_secs
    );
}

#[test]
fn packets_before_first_keyframe_are_dropped_and_counted() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let src = source();
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let mut m =
        SegmentMuxer::new(cfg(&out_dir, 2.0, VideoCodec::H264, vec![track("Game")])).expect("new");
    // Start at AU 5 (non-keyframe); AUs 5..=59 are before the keyframe at 60.
    feed(&mut m, &src, 5, 1);
    assert_eq!(m.dropped_before_keyframe(), 55, "dropped count");
    let paths = m.finish().expect("finish");
    assert_eq!(paths.len(), 2, "AUs 60..179 = 4 s = 2 segments");

    let csv = fs::read_to_string(out_dir.join("segments.csv")).expect("csv");
    let first: f64 = csv
        .lines()
        .next()
        .unwrap()
        .split(',')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        first.abs() < 1e-6,
        "timeline starts at first keyframe, got {first}"
    );

    for (k, seg) in paths.iter().enumerate() {
        let mv = read_movie(seg).expect("read segment");
        let v = mv.tracks.iter().find(|t| t.is_video()).expect("video");
        assert_video(v, 1280, 720, 60, &format!("seg {k}"));
        assert!(
            v.samples[0].keyframe,
            "seg {k} first frame must be keyframe"
        );
        assert_decodes_clean(seg, 60);
    }
}

#[test]
fn no_packets_yields_no_segments_and_no_csv() {
    let dir = tempfile::tempdir().unwrap();
    let m = SegmentMuxer::new(cfg(dir.path(), 2.0, VideoCodec::H264, vec![])).expect("new");
    assert_eq!(m.dropped_before_keyframe(), 0);
    let paths = m.finish().expect("finish");
    assert!(paths.is_empty(), "paths: {paths:?}");
    assert!(
        !dir.path().join("segments.csv").exists(),
        "csv must not exist"
    );
}

#[test]
fn push_audio_out_of_range_track_is_bad_track() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = SegmentMuxer::new(cfg(dir.path(), 2.0, VideoCodec::H264, vec![track("Game")]))
        .expect("new");
    let r = m.push_audio(
        5,
        AudioPacket {
            data: vec![0u8; 8],
            pts: 0,
            samples: 1024,
        },
    );
    let dbg = format!("{r:?}");
    assert!(
        r.is_err() && dbg.contains("BadTrack"),
        "expected BadTrack, got {dbg}"
    );
}
