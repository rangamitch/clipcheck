//! Tests for the default-audio ordering: the first logical audio track ("Mix", track id 2) is
//! written LAST in the moov so Windows plays it by default. Logical order (track ids) must be
//! preserved by the reader and by `concat_remux`.
//!
//! Synthetic media only (no native media generator), so these run everywhere.

use clipcheck_mp4::aac::aac_lc_asc;
use clipcheck_mp4::avc::avc_config;
use clipcheck_mp4::fmp4::{
    write_fragment, write_init, AudioTrackSpec, InitSpec, Sample, TrackRun, VideoConfig,
    VideoTrackSpec,
};
use clipcheck_mp4::reader::{read_movie, read_movie_from, Movie, TrackKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// Annex B SPS + PPS of a 1280x720 High profile stream.
const PARAM_SETS: &[u8] = &[
    0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10,
    0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60, 0x00, 0x00,
    0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0,
];

fn audio_spec(title: &str) -> AudioTrackSpec {
    AudioTrackSpec {
        sample_rate: 48_000,
        channels: 2,
        asc: aac_lc_asc(48_000, 2).expect("asc"),
        bitrate: 128_000,
        title: title.to_string(),
    }
}

fn init_spec(titles: &[&str]) -> InitSpec {
    InitSpec {
        video: VideoTrackSpec {
            config: VideoConfig::Avc(avc_config(PARAM_SETS).expect("sps and pps")),
            width: 1280,
            height: 720,
            timescale: 90_000,
        },
        audio: titles.iter().map(|t| audio_spec(t)).collect(),
    }
}

fn sample(data: &[u8], duration: u32, keyframe: bool) -> Sample {
    Sample {
        data: data.to_vec(),
        duration,
        cts_offset: 0,
        keyframe,
    }
}

/// One fragment: 2 video samples (track 1) and 2 AAC-like samples per audio track (ids 2..).
fn fragment(n_audio: usize, seq: u32) -> Vec<u8> {
    let mut runs = vec![TrackRun {
        track_id: 1,
        base_decode_time: 0,
        samples: vec![
            sample(&[1u8; 10], 3000, true),
            sample(&[2u8; 20], 3000, false),
        ],
    }];
    for i in 0..n_audio {
        runs.push(TrackRun {
            track_id: i as u32 + 2,
            base_decode_time: 0,
            samples: vec![
                sample(&[0xA0 + i as u8; 5], 1024, true),
                sample(&[0xB0 + i as u8; 7], 1024, true),
            ],
        });
    }
    write_fragment(seq, &runs)
}

/// Init + one fragment, as a single byte vector.
fn media(titles: &[&str]) -> Vec<u8> {
    let init = write_init(&init_spec(titles));
    let frag = fragment(titles.len(), 1);
    [init, frag].concat()
}

/// Track ids in raw `tkhd` order of the file. Track id is the u32 at payload offset 12
/// (version/flags 4 + creation 4 + modification 4), i.e. 16 bytes after the tkhd fourcc position.
fn raw_tkhd_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    for i in 0..bytes.len().saturating_sub(4) {
        if &bytes[i..i + 4] == b"tkhd" {
            let at = i + 16;
            let id = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            ids.push(id);
        }
    }
    ids
}

/// (track id, title) of every track in parsed order.
fn ids_and_titles(m: &Movie) -> Vec<(u32, Option<String>)> {
    m.tracks.iter().map(|t| (t.id, t.title.clone())).collect()
}

fn audio_titles(m: &Movie) -> Vec<String> {
    m.tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio { .. }))
        .map(|t| t.title.clone().unwrap_or_default())
        .collect()
}

fn parse(bytes: &[u8]) -> Movie {
    read_movie_from(Cursor::new(bytes.to_vec())).expect("reader parses bytes")
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write input");
    p
}

fn remux_one(input: &Path, out: &Path) -> Vec<u8> {
    concat_remux(&[input.to_path_buf()], out, &RemuxOptions::default()).expect("concat_remux");
    std::fs::read(out).expect("read output")
}

#[test]
fn init_writes_first_audio_trak_last() {
    let init = write_init(&init_spec(&["Mix", "Game", "Microphone"]));
    assert_eq!(raw_tkhd_ids(&init), vec![1, 3, 4, 2]);
}

#[test]
fn reader_returns_logical_order_for_three_audio_tracks() {
    let m = parse(&media(&["Mix", "Game", "Microphone"]));
    assert_eq!(
        ids_and_titles(&m),
        vec![
            (1, None),
            (2, Some("Mix".to_string())),
            (3, Some("Game".to_string())),
            (4, Some("Microphone".to_string())),
        ]
    );
}

#[test]
fn remux_preserves_logical_order_and_puts_mix_last_in_raw_order() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_file(dir.path(), "in.mp4", &media(&["Mix", "Game", "Microphone"]));
    let out = dir.path().join("out.mp4");
    let bytes = remux_one(&input, &out);

    let m = parse(&bytes);
    assert_eq!(
        ids_and_titles(&m),
        vec![
            (1, None),
            (2, Some("Mix".to_string())),
            (3, Some("Game".to_string())),
            (4, Some("Microphone".to_string())),
        ],
        "parsed remux output order"
    );
    assert_eq!(
        raw_tkhd_ids(&bytes),
        vec![1, 3, 4, 2],
        "raw tkhd order of remux output (first audio last)"
    );
}

#[test]
fn remux_twice_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_file(dir.path(), "in.mp4", &media(&["Mix", "Game", "Microphone"]));
    let once_path = dir.path().join("once.mp4");
    let once = remux_one(&input, &once_path);
    let twice_path = dir.path().join("twice.mp4");
    let twice = remux_one(&once_path, &twice_path);

    assert_eq!(raw_tkhd_ids(&twice), raw_tkhd_ids(&once), "raw tkhd order");
    assert_eq!(raw_tkhd_ids(&twice), vec![1, 3, 4, 2]);
    let m = parse(&twice);
    assert_eq!(audio_titles(&m), vec!["Mix", "Game", "Microphone"]);
    assert_eq!(
        m.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn one_audio_track_is_unchanged() {
    let bytes = media(&["Mix"]);
    assert_eq!(raw_tkhd_ids(&bytes), vec![1, 2], "raw tkhd order");
    let m = parse(&bytes);
    assert_eq!(
        ids_and_titles(&m),
        vec![(1, None), (2, Some("Mix".to_string()))]
    );

    let dir = tempfile::tempdir().unwrap();
    let input = write_file(dir.path(), "in.mp4", &bytes);
    let out = remux_one(&input, &dir.path().join("out.mp4"));
    assert_eq!(raw_tkhd_ids(&out), vec![1, 2], "remux raw tkhd order");
    assert_eq!(audio_titles(&parse(&out)), vec!["Mix"]);
}

#[test]
fn zero_audio_tracks_still_works() {
    let bytes = media(&[]);
    assert_eq!(raw_tkhd_ids(&bytes), vec![1], "raw tkhd order");
    let m = parse(&bytes);
    assert_eq!(m.tracks.len(), 1);
    assert_eq!(m.tracks[0].id, 1);
    assert!(m.tracks[0].is_video());

    let dir = tempfile::tempdir().unwrap();
    let input = write_file(dir.path(), "in.mp4", &bytes);
    let out = remux_one(&input, &dir.path().join("out.mp4"));
    let m2 = parse(&out);
    assert_eq!(m2.tracks.len(), 1);
    assert_eq!(m2.tracks[0].id, 1);
    assert_eq!(raw_tkhd_ids(&out), vec![1]);
}

#[test]
fn path_entry_point_matches_logical_order() {
    let dir = tempfile::tempdir().unwrap();
    let input = write_file(dir.path(), "in.mp4", &media(&["Mix", "Game"]));
    let m = read_movie(&input).expect("read_movie");
    assert_eq!(
        m.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(audio_titles(&m), vec!["Mix", "Game"]);
}
