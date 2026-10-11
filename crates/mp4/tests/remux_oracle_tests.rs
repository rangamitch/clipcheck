//! Oracle tests for `clipcheck_mp4::remux::concat_remux`. Fixtures are generated natively by
//! `clipcheck-testkit` (Media Foundation); the output is judged by `clipcheck_mp4::reader`
//! (structure, sample counts, timing) and by decoding through `clipcheck_mfdec`.
//!
//! Skips (prints SKIPPED and returns) when native test media is unavailable.

#![cfg(windows)]

use clipcheck_mfdec::{probe, OutFormat, VideoDecoder};
use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions, TrackSelect};
use clipcheck_mp4::Mp4Error;
use clipcheck_testkit::{write_mp4, write_segments, AudioSpec, MediaSpec, VideoSpec};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

macro_rules! need {
    () => {
        if !clipcheck_testkit::available() {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        }
    };
}

/// Sample count and summed sample duration (seconds) of one track.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Stat {
    samples: usize,
    secs: f64,
}

fn stat(t: &Track) -> Stat {
    Stat {
        samples: t.samples.len(),
        secs: t.samples.iter().map(|s| s.duration as f64).sum::<f64>() / t.timescale as f64,
    }
}

fn stats(m: &Movie) -> Vec<Stat> {
    m.tracks.iter().map(stat).collect()
}

/// Per-track totals over all inputs (same track layout in every input).
fn sum_inputs(segs: &[PathBuf]) -> Vec<Stat> {
    let mut acc: Vec<Stat> = vec![];
    for p in segs {
        let s = stats(&read_movie(p).expect("reader on input"));
        if acc.is_empty() {
            acc = vec![Stat::default(); s.len()];
        }
        assert_eq!(s.len(), acc.len(), "input track layout differs");
        for (a, x) in acc.iter_mut().zip(s) {
            a.samples += x.samples;
            a.secs += x.secs;
        }
    }
    acc
}

fn video_track(m: &Movie) -> &Track {
    m.tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .expect("video track")
}

fn audio_tracks(m: &Movie) -> Vec<&Track> {
    m.tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio { .. }))
        .collect()
}

fn part_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".part"))
        .collect()
}

/// Three 2 s 720p30 segments with keyframes every 2 s and two titled audio tracks
/// (Game, Mic), like a ClipCheck recording.
fn set_spec(secs: f64) -> MediaSpec {
    MediaSpec {
        secs,
        video: Some(VideoSpec {
            width: 1280,
            height: 720,
            fps: 30,
            gop_frames: 60,
        }),
        audio: vec![
            AudioSpec::titled(440.0, "Game"),
            AudioSpec::titled(880.0, "Mic"),
        ],
    }
}

fn make_set(dir: &Path) -> Vec<PathBuf> {
    let segs = write_segments(dir, &set_spec(6.0), 2.0).expect("write_segments");
    assert_eq!(segs.len(), 3, "segment count");
    segs
}

/// FNV-1a over a byte slice.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// FNV-1a of every decoded NV12 frame of `p`, in presentation order. Panics on any decode error.
fn decode_hashes(p: &Path) -> Vec<u64> {
    let mut d = VideoDecoder::open(p, OutFormat::Nv12).expect("open decoder");
    let mut out = vec![];
    while let Some(f) = d.next_frame().expect("decode frame") {
        out.push(fnv1a(&f.data));
    }
    out
}

/// Decoded frame hashes of every input, concatenated in input order.
fn input_hashes(segs: &[PathBuf]) -> Vec<u64> {
    segs.iter().flat_map(|p| decode_hashes(p)).collect()
}

/// Asserts equal frame sequences, naming the first mismatching frame.
fn assert_frames_eq(ours: &[u64], theirs: &[u64], what: &str) {
    assert_eq!(ours.len(), theirs.len(), "{what}: frame count");
    if let Some(i) = ours.iter().zip(theirs).position(|(a, b)| a != b) {
        panic!("{what}: first differing frame is {i}");
    }
}

/// Top-level box types in file order.
fn top_boxes(path: &Path) -> Vec<String> {
    let b = fs::read(path).unwrap();
    let mut out = vec![];
    let mut i = 0usize;
    while i + 8 <= b.len() {
        let size32 = u32::from_be_bytes(b[i..i + 4].try_into().unwrap());
        let typ = String::from_utf8_lossy(&b[i + 4..i + 8]).to_string();
        let size = match size32 {
            1 => u64::from_be_bytes(b[i + 8..i + 16].try_into().unwrap()) as usize,
            0 => b.len() - i,
            n => n as usize,
        };
        out.push(typ);
        if size < 8 {
            break;
        }
        i += size;
    }
    out
}

/// Number of video samples whose DTS does not strictly increase.
fn dts_violations(t: &Track) -> usize {
    t.samples
        .windows(2)
        .filter(|w| w[1].dts <= w[0].dts)
        .count()
}

#[test]
fn t1_concat_three_segments() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let segs = make_set(dir.path());
    let out = dir.path().join("out.mp4");
    let rep = concat_remux(&segs, &out, &RemuxOptions::default()).expect("concat");
    println!("T1 report: {rep:?}");
    assert_eq!(
        rep.bytes,
        fs::metadata(&out).unwrap().len(),
        "reported bytes"
    );
    assert_eq!(rep.actual_start_secs, 0.0);

    let m = read_movie(&out).expect("reader");
    assert_eq!(m.tracks.len(), 3, "track count");
    match &video_track(&m).kind {
        TrackKind::Video {
            codec,
            width,
            height,
        } => {
            assert_eq!(codec, "h264");
            assert_eq!((*width, *height), (1280, 720));
        }
        other => panic!("not video: {other:?}"),
    }

    let want = sum_inputs(&segs);
    let got = stats(&m);
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        println!("T1 track {i}: out {g:?} inputs {w:?}");
        assert_eq!(g.samples, w.samples, "track {i} sample count vs inputs");
        assert!(
            (g.secs - w.secs).abs() <= 0.05,
            "track {i} duration {} vs inputs {}",
            g.secs,
            w.secs
        );
    }

    let titles: Vec<String> = audio_tracks(&m)
        .iter()
        .map(|t| t.title.clone().unwrap_or_default())
        .collect();
    assert_eq!(titles, vec!["Game".to_string(), "Mic".to_string()]);

    assert_eq!(
        dts_violations(video_track(&m)),
        0,
        "video DTS not increasing"
    );
    let boxes = top_boxes(&out);
    println!("T1 top-level boxes: {boxes:?}");
    let moov = boxes.iter().position(|b| b == "moov").expect("moov");
    let mdat = boxes.iter().position(|b| b == "mdat").expect("mdat");
    assert!(moov < mdat, "moov must precede mdat (faststart)");
    assert!(part_files(dir.path()).is_empty(), "no .part left");
}

#[test]
fn t2_clip_like_the_app() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let segs = make_set(dir.path());

    // start inside segment 0 -> snaps back to 0
    let out = dir.path().join("a.mp4");
    let r = concat_remux(
        &segs,
        &out,
        &RemuxOptions {
            start_secs: 1.3,
            ..Default::default()
        },
    )
    .expect("start 1.3");
    println!("T2 start 1.3 report: {r:?}");
    assert!(
        r.actual_start_secs.abs() < 1e-6,
        "actual {}",
        r.actual_start_secs
    );

    // start 2.4 -> keyframe 2.0, 4 s remain (120 frames)
    let out = dir.path().join("b.mp4");
    let r = concat_remux(
        &segs,
        &out,
        &RemuxOptions {
            start_secs: 2.4,
            ..Default::default()
        },
    )
    .expect("start 2.4");
    println!("T2 start 2.4 report: {r:?}");
    // The AAC priming shift (1024 samples = 0.0213 s) is carried into the output, so
    // allow one priming period.
    assert!(
        (r.actual_start_secs - 2.0).abs() < 0.025,
        "actual {}",
        r.actual_start_secs
    );
    let m = read_movie(&out).expect("reader");
    let v = video_track(&m);
    let vs = stat(v);
    println!("T2 start 2.4 video {vs:?}");
    assert!((vs.secs - 4.0).abs() <= 0.1, "duration {}", vs.secs);
    assert!(v.samples[0].keyframe, "first video sample not a keyframe");
    assert_eq!(decode_hashes(&out).len(), 120, "decoded frames");
    assert_eq!(dts_violations(v), 0, "video DTS not increasing");

    // start 2.4 end 5.0 -> 90 frames, 3.0 s
    let out = dir.path().join("c.mp4");
    let r = concat_remux(
        &segs,
        &out,
        &RemuxOptions {
            start_secs: 2.4,
            end_secs: Some(5.0),
            ..Default::default()
        },
    )
    .expect("start 2.4 end 5.0");
    println!("T2 start 2.4 end 5.0 report: {r:?}");
    let m = read_movie(&out).expect("reader");
    let v = video_track(&m);
    let vs = stat(v);
    let n = decode_hashes(&out).len();
    println!("T2 2.4..5.0 video {vs:?} decoded frames {n}");
    assert!((vs.secs - 3.0).abs() <= 0.05, "duration {}", vs.secs);
    // Arithmetic says 90. The AAC priming offset can cost one frame at the window edge.
    assert!((n as i64 - 90).abs() <= 1, "frames {n}");
    assert_eq!(dts_violations(v), 0, "video DTS not increasing");
    assert!(v.samples[0].keyframe, "first video sample not a keyframe");
}

#[test]
fn t3_decoded_frames_match_inputs() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let segs = make_set(dir.path());
    let ours = dir.path().join("ours.mp4");
    concat_remux(&segs, &ours, &RemuxOptions::default()).expect("concat");

    let theirs = input_hashes(&segs);
    let got = decode_hashes(&ours);
    println!(
        "T3 decoded frames ours={} inputs={}",
        got.len(),
        theirs.len()
    );
    assert_frames_eq(&got, &theirs, "decoded output vs concatenated inputs");
    // Seam frames: last of segment 0, first of segment 1.
    for n in [59usize, 60] {
        assert_eq!(got[n], theirs[n], "seam frame {n}");
    }

    let m = read_movie(&ours).expect("reader");
    let want = sum_inputs(&segs);
    let vd = stat(video_track(&m)).secs;
    assert!(
        (vd - want[0].secs).abs() <= 0.05,
        "video duration {vd} vs inputs {}",
        want[0].secs
    );
}

#[test]
fn t4_audio_only() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let segs = make_set(dir.path());
    let input_titles: Vec<Option<String>> = read_movie(&segs[0])
        .expect("reader on input")
        .tracks
        .iter()
        .map(|t| t.title.clone())
        .collect();
    println!("T4 input track titles: {input_titles:?}");
    // Track 0 is video; index 1 is the Game audio track (the `game.m4a` name).
    assert_eq!(
        input_titles[1].as_deref(),
        Some("Game"),
        "input precondition"
    );
    let out = dir.path().join("game.m4a");
    concat_remux(
        &segs,
        &out,
        &RemuxOptions {
            tracks: TrackSelect::Only(vec![1]),
            ..Default::default()
        },
    )
    .expect("audio only");

    let m = read_movie(&out).expect("reader");
    assert_eq!(m.tracks.len(), 1, "one track");
    match &m.tracks[0].kind {
        TrackKind::Audio {
            codec, sample_rate, ..
        } => {
            assert_eq!(codec, "aac");
            assert_eq!(*sample_rate, 48_000);
        }
        other => panic!("not audio: {other:?}"),
    }
    assert_eq!(
        m.tracks[0].title.as_deref(),
        Some("Game"),
        "kept track title"
    );
    let want = sum_inputs(&segs)[1];
    let got = stat(&m.tracks[0]);
    println!("T4 out {got:?} inputs(track 1) {want:?}");
    assert_eq!(got.samples, want.samples, "audio sample count vs inputs");
    assert!(
        (got.secs - want.secs).abs() <= 0.05,
        "audio duration {} vs inputs {}",
        got.secs,
        want.secs
    );

    let p = probe(&out).expect("mfdec probe");
    assert_eq!(p.audio.len(), 1, "probed audio streams");
    assert!(
        p.audio[0].codec.eq_ignore_ascii_case("aac"),
        "codec {}",
        p.audio[0].codec
    );
    assert_eq!(p.audio[0].sample_rate, 48_000);
}

#[test]
fn t5_flat_trim() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    // One flat file, keyframe every 2 s (GOP 60 at 30 fps), video only.
    let flat = dir.path().join("flat.mp4");
    let spec = MediaSpec {
        secs: 3.0,
        video: Some(VideoSpec {
            width: 1280,
            height: 720,
            fps: 30,
            gop_frames: 60,
        }),
        audio: vec![],
    };
    write_mp4(&flat, &spec).expect("flat gen");

    let out = dir.path().join("trim.mp4");
    let r = concat_remux(
        std::slice::from_ref(&flat),
        &out,
        &RemuxOptions {
            start_secs: 1.0,
            end_secs: Some(2.5),
            ..Default::default()
        },
    )
    .expect("trim");
    println!("T5 report: {r:?}");
    assert!(
        r.actual_start_secs.abs() < 1e-6,
        "start snaps to keyframe 0"
    );
    let frames = decode_hashes(&out).len();
    println!("T5 frames {frames} (expected ~75: keyframe at 0 snaps start back)");
    assert!((frames as i64 - 75).abs() <= 1, "frames {frames}");
    let m = read_movie(&out).expect("reader");
    assert_eq!(
        dts_violations(video_track(&m)),
        0,
        "video DTS not increasing"
    );
}

#[test]
fn t6_errors() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let segs = make_set(dir.path());
    // One audio track instead of two: layout mismatch with the set.
    let odd = dir.path().join("odd.mp4");
    let odd_spec = MediaSpec {
        secs: 2.0,
        audio: vec![AudioSpec::sine(440.0)],
        ..set_spec(2.0)
    };
    write_mp4(&odd, &odd_spec).expect("odd gen");

    // mismatched layouts
    let out = dir.path().join("mismatch.mp4");
    fs::write(&out, b"SENTINEL").unwrap();
    let e = concat_remux(
        &[segs[0].clone(), odd.clone()],
        &out,
        &RemuxOptions::default(),
    );
    println!("T6 mismatch: {e:?}");
    assert!(matches!(e, Err(Mp4Error::InvalidData(_))), "got {e:?}");
    assert_eq!(
        fs::read(&out).unwrap(),
        b"SENTINEL",
        "existing out modified"
    );
    assert!(
        part_files(dir.path()).is_empty(),
        "stray .part after mismatch"
    );

    // empty inputs
    let out2 = dir.path().join("empty.mp4");
    let e = concat_remux(&[], &out2, &RemuxOptions::default());
    println!("T6 empty: {e:?}");
    assert!(e.is_err());
    assert!(!out2.exists(), "out created on empty input");
    assert!(part_files(dir.path()).is_empty());

    // bad ranges
    for (st, en) in [(2.0, Some(2.0)), (3.0, Some(1.0))] {
        let e = concat_remux(
            &segs,
            &out,
            &RemuxOptions {
                start_secs: st,
                end_secs: en,
                ..Default::default()
            },
        );
        println!("T6 range {st}..{en:?}: {e:?}");
        assert!(matches!(e, Err(Mp4Error::InvalidConfig(_))), "got {e:?}");
    }
    assert_eq!(
        fs::read(&out).unwrap(),
        b"SENTINEL",
        "out modified by bad options"
    );
    assert!(part_files(dir.path()).is_empty());
}

#[test]
fn t7_large_concat_timing() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    // 1080p60, three 20 s segments, keyframe every second, two audio tracks.
    let spec = MediaSpec {
        secs: 60.0,
        video: Some(VideoSpec {
            width: 1920,
            height: 1080,
            fps: 60,
            gop_frames: 60,
        }),
        audio: vec![AudioSpec::sine(440.0), AudioSpec::sine(880.0)],
    };
    let segs = write_segments(dir.path(), &spec, 20.0).expect("write_segments");
    assert_eq!(segs.len(), 3, "segment count");

    let out = dir.path().join("big_out.mp4");
    let start = Instant::now();
    let r = concat_remux(&segs, &out, &RemuxOptions::default()).expect("large concat");
    let el = start.elapsed();
    println!(
        "T7 1080p60 3x20s concat: {:.3}s, output {} bytes, duration {:.2}s",
        el.as_secs_f64(),
        r.bytes,
        r.duration_secs
    );
    let want = sum_inputs(&segs);
    let got_hashes = decode_hashes(&out);
    let want_hashes = input_hashes(&segs);
    println!(
        "T7 frames ours={} inputs={}",
        got_hashes.len(),
        want_hashes.len()
    );
    assert_frames_eq(
        &got_hashes,
        &want_hashes,
        "1080p60 decoded output vs inputs",
    );
    assert!(
        (r.duration_secs - want[0].secs).abs() <= 0.05,
        "duration {} vs inputs {}",
        r.duration_secs,
        want[0].secs
    );
    let m = read_movie(&out).expect("reader");
    assert_eq!(
        dts_violations(video_track(&m)),
        0,
        "video DTS not increasing"
    );
}
