//! Integration tests for `clipcheck_engine::sessionfile`.
//!
//! Fixtures are generated with `clipcheck-testkit` into per-test temp dirs, so nothing is
//! written into the repository tree. When native test media is unavailable, each test prints
//! `SKIPPED: native test media unavailable` and returns.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[macro_use]
mod common;

use clipcheck_engine::sessionfile::{recover, FinalizedPart, SessionAssembler};
use clipcheck_testkit::{AudioSpec, MediaSpec};
use common::{copy_into, fixture_dir, session_segments, SEGMENT_COUNT};

/// Tolerance for the zero-gap checks: one frame at the fixture rate. The
/// session fixture is encoded at 30 fps (`rate=30` in `common`), so one frame is
/// 1/30 s. A gap of one frame or more fails. `FLOAT_EPS` absorbs f64 rounding
/// in the MP4 reader's output and the sums.
const FRAME_TOL: f64 = 1.0 / 30.0;
const FLOAT_EPS: f64 = 1e-6;

/// Skips the enclosing test when native test media is unavailable. Every test that
/// writes media starts with this.
macro_rules! require_media {
    () => {
        if !clipcheck_testkit::available() {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        }
    };
}

/// Identifies the file behind `path`: volume serial and file index on Windows,
/// device and inode elsewhere. Two paths name one file when these match.
#[cfg(windows)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut c_void, info: *mut [u32; 13]) -> i32;
    }

    let file = fs::File::open(path).ok()?;
    // BY_HANDLE_FILE_INFORMATION, 52 bytes: attributes, three FILETIMEs,
    // volume serial (index 7), size (8, 9), link count (10), file index
    // high (11) and low (12).
    let mut info = [0u32; 13];
    // SAFETY: `file` owns a valid handle for the duration of the call, and
    // `info` has the 52-byte layout the API writes.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return None;
    }
    let index = (u64::from(info[11]) << 32) | u64::from(info[12]);
    Some((u64::from(info[7]), index))
}

/// Identifies the file behind `path` by device and inode.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path).ok()?;
    Some((m.dev(), m.ino()))
}

/// True when `a` and `b` are one file on disk, not two copies.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((file_id(a), file_id(b)), (Some(x), Some(y)) if x == y)
}

/// Generates the six source segments in `<root>/source` by copying the cached
/// session fixture. The copies are independent, so tests may change them.
fn make_segments(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("source");
    fs::create_dir_all(&dir).expect("create source dir");
    copy_into(session_segments(), &dir)
}

/// Staged `part-*.mp4` file names in `stage`.
fn staged_parts(stage: &Path) -> Vec<String> {
    if !stage.exists() {
        return Vec::new();
    }
    let mut v: Vec<String> = fs::read_dir(stage)
        .expect("read stage dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("part-") && n.ends_with(".mp4"))
        .collect();
    v.sort();
    v
}

fn assert_stage_clean(stage: &Path) {
    assert!(
        staged_parts(stage).is_empty(),
        "staged parts left behind: {:?}",
        staged_parts(stage)
    );
    assert!(
        !stage.join("concat.txt").exists(),
        "concat list left in stage dir"
    );
}

fn tmp_sibling(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".tmp.mp4");
    PathBuf::from(s)
}

fn probe_duration(path: &Path) -> f64 {
    clipcheck_mp4::reader::read_movie(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .duration_secs
}

fn assert_duration_between(path: &Path, lo: f64, hi: f64) {
    let d = probe_duration(path);
    assert!(
        (lo..=hi).contains(&d),
        "duration of {} is {d:.3}s, expected {lo}..={hi}",
        path.display()
    );
}

fn first_video_packet_is_keyframe(path: &Path) -> bool {
    let movie = clipcheck_mp4::reader::read_movie(path).expect("read mp4");
    let video = movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .expect("no video track");
    video.samples.first().expect("no video samples").keyframe
}

/// Top-level MP4 atoms as `(type, offset)`, handling 64-bit largesize and
/// the size-0 "to end of file" form.
fn top_level_atoms(data: &[u8]) -> Vec<([u8; 4], u64)> {
    let len = data.len() as u64;
    let mut atoms = Vec::new();
    let mut off: u64 = 0;
    while off + 8 <= len {
        let o = off as usize;
        let size32 = u32::from_be_bytes(data[o..o + 4].try_into().unwrap()) as u64;
        let typ: [u8; 4] = data[o + 4..o + 8].try_into().unwrap();
        let size = match size32 {
            1 => {
                assert!(off + 16 <= len, "truncated largesize atom at {off}");
                u64::from_be_bytes(data[o + 8..o + 16].try_into().unwrap())
            }
            0 => len - off,
            s => s,
        };
        assert!(
            size >= 8,
            "invalid atom size {size} for {} at {off}",
            String::from_utf8_lossy(&typ)
        );
        atoms.push((typ, off));
        off += size;
    }
    atoms
}

fn atom_offset(atoms: &[([u8; 4], u64)], name: &[u8; 4]) -> Option<u64> {
    atoms.iter().find(|(t, _)| t == name).map(|(_, o)| *o)
}

#[test]
fn six_segments_finalize_to_12s_faststart_keyframe_start() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    let mut asm = SessionAssembler::new(stage.clone()).expect("new");
    for s in &segs {
        asm.add_segment(s).expect("add_segment");
    }
    assert_eq!(asm.segment_count(), SEGMENT_COUNT);

    let part: FinalizedPart = asm.finalize(&out).expect("finalize");
    assert_eq!(part.segments, SEGMENT_COUNT, "segments");
    assert_eq!(part.path, out, "path");
    assert!(out.is_file(), "out file missing");
    let len = fs::metadata(&out).expect("metadata").len();
    assert_eq!(part.bytes, len, "bytes does not match file length");
    assert!(!tmp_sibling(&out).exists(), "tmp file left behind");
    assert_stage_clean(&stage);

    assert_duration_between(&out, 11.8, 12.3);

    let data = fs::read(&out).expect("read out");
    let atoms = top_level_atoms(&data);
    let moov = atom_offset(&atoms, b"moov").expect("no moov atom");
    let mdat = atom_offset(&atoms, b"mdat").expect("no mdat atom");
    assert!(
        moov < mdat,
        "moov ({moov}) must precede mdat ({mdat}) for faststart; atoms: {:?}",
        atoms
            .iter()
            .map(|(t, _)| String::from_utf8_lossy(t).into_owned())
            .collect::<Vec<_>>()
    );

    assert!(
        first_video_packet_is_keyframe(&out),
        "first video packet is not a keyframe"
    );
}

#[test]
fn split_flow_is_zero_gap() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out_a = tmp.path().join("a.mp4");
    let out_b = tmp.path().join("b.mp4");

    let mut asm = SessionAssembler::new(stage.clone()).expect("new");
    for s in &segs[..3] {
        asm.add_segment(s).expect("add a");
    }
    let a = asm.finalize(&out_a).expect("finalize a");
    assert_eq!(a.segments, 3);
    assert_eq!(asm.segment_count(), 0, "count after first finalize");

    for s in &segs[3..] {
        asm.add_segment(s).expect("add b");
    }
    let b = asm.finalize(&out_b).expect("finalize b");
    assert_eq!(b.segments, 3);
    assert_eq!(asm.segment_count(), 0, "count after second finalize");

    assert!(out_a.is_file() && out_b.is_file(), "both parts must exist");
    assert_stage_clean(&stage);

    // Part A is segs[..3] and part B is segs[3..]. Each part must equal the
    // summed duration of its own source segments, so the parts together cover
    // the source with no gap and no overlap. Tolerance is one frame.
    let tol = FRAME_TOL + FLOAT_EPS;
    let src_a: f64 = segs[..3].iter().map(|s| probe_duration(s)).sum();
    let src_b: f64 = segs[3..].iter().map(|s| probe_duration(s)).sum();
    let da = probe_duration(&out_a);
    let db = probe_duration(&out_b);
    assert!(
        (da - src_a).abs() <= tol,
        "part A duration {da:.3}s, sources {src_a:.3}s (tol {tol:.4}s)"
    );
    assert!(
        (db - src_b).abs() <= tol,
        "part B duration {db:.3}s, sources {src_b:.3}s (tol {tol:.4}s)"
    );
    assert!(
        ((da + db) - (src_a + src_b)).abs() <= tol,
        "A + B duration {:.3}s, sources {:.3}s",
        da + db,
        src_a + src_b
    );
    assert!(
        first_video_packet_is_keyframe(&out_b),
        "part B does not start on a keyframe"
    );
}

#[test]
fn hardlink_survives_deletion_of_originals() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    let mut asm = SessionAssembler::new(stage.clone()).expect("new");
    for s in &segs {
        asm.add_segment(s).expect("add_segment");
    }
    // Staging must hard-link: each staged part is the same file as its source.
    // A fallback copy would still pass the checks below, so check identity here.
    let staged = staged_parts(&stage);
    assert_eq!(staged.len(), SEGMENT_COUNT, "staged part count");
    for (s, name) in segs.iter().zip(&staged) {
        assert!(
            same_file(s, &stage.join(name)),
            "staged {name} is a copy of {}, not a hard link",
            s.display()
        );
    }
    for s in &segs {
        fs::remove_file(s).expect("delete original segment");
    }

    let part = asm
        .finalize(&out)
        .expect("finalize after deleting originals");
    assert_eq!(part.segments, SEGMENT_COUNT);
    assert_duration_between(&out, 11.8, 12.3);
}

#[test]
fn staged_segment_count() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    let mut asm = SessionAssembler::new(stage).expect("new");
    assert_eq!(asm.segment_count(), 0);

    asm.add_segment(&segs[0]).expect("add");
    assert_eq!(asm.segment_count(), 1);

    asm.add_segment(&segs[1]).expect("add");
    asm.add_segment(&segs[2]).expect("add");
    assert_eq!(asm.segment_count(), 3);

    asm.finalize(&out).expect("finalize");
    assert_eq!(asm.segment_count(), 0);
}

#[test]
fn finalize_empty_is_invalid_input() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    let mut asm = SessionAssembler::new(stage.clone()).expect("new");
    let err = asm
        .finalize(&out)
        .expect_err("finalize with no segments must fail");
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "error: {err}");
    assert!(!out.exists(), "out file must not be written");
    assert!(!tmp_sibling(&out).exists(), "tmp file must not be written");
}

#[test]
fn recover_assembles_leftover_parts() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("recovered.mp4");

    {
        // Simulated crash: stage four segments, never finalize.
        let mut asm = SessionAssembler::new(stage.clone()).expect("new");
        for s in &segs[..4] {
            asm.add_segment(s).expect("add");
        }
    }
    assert_eq!(staged_parts(&stage).len(), 4);

    let part = recover(stage.clone(), &out)
        .expect("recover")
        .expect("recover should assemble leftover parts");
    assert_eq!(part.segments, 4);
    assert_eq!(part.path, out);
    assert_duration_between(&out, 7.7, 8.3);
    assert_stage_clean(&stage);

    let again = recover(stage.clone(), &out).expect("second recover");
    assert!(again.is_none(), "empty stage dir must yield Ok(None)");

    let missing = tmp.path().join("no_such_stage");
    let none = recover(missing.clone(), &tmp.path().join("x.mp4")).expect("recover on missing dir");
    assert!(none.is_none(), "missing stage dir must yield Ok(None)");
    assert!(!missing.exists(), "recover must not create the stage dir");
}

#[test]
fn new_adopts_existing_parts_and_continues_numbering() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    {
        let mut asm = SessionAssembler::new(stage.clone()).expect("new");
        asm.add_segment(&segs[0]).expect("add");
        asm.add_segment(&segs[1]).expect("add");
    }

    let mut asm = SessionAssembler::new(stage.clone()).expect("reopen");
    assert_eq!(asm.segment_count(), 2, "existing parts must be adopted");

    asm.add_segment(&segs[2]).expect("add");
    assert_eq!(
        staged_parts(&stage),
        vec![
            "part-000000.mp4".to_string(),
            "part-000001.mp4".to_string(),
            "part-000002.mp4".to_string(),
        ],
        "numbering must continue past adopted parts"
    );

    let part = asm.finalize(&out).expect("finalize");
    assert_eq!(part.segments, 3);
    assert_duration_between(&out, 5.7, 6.3);
}

#[test]
fn source_segments_untouched() {
    require_media!();
    let tmp = tempfile::tempdir().expect("tempdir");
    let segs = make_segments(tmp.path());
    let stage = tmp.path().join("stage");
    let out = tmp.path().join("session.mp4");

    let before: Vec<u64> = segs
        .iter()
        .map(|s| fs::metadata(s).expect("metadata").len())
        .collect();

    let mut asm = SessionAssembler::new(stage).expect("new");
    for s in &segs {
        asm.add_segment(s).expect("add");
    }
    asm.finalize(&out).expect("finalize");

    for (s, len) in segs.iter().zip(before) {
        assert!(s.is_file(), "source segment {} was deleted", s.display());
        assert_eq!(
            fs::metadata(s).expect("metadata").len(),
            len,
            "source segment {} was modified",
            s.display()
        );
    }
}

/// The cached audio fixture for `titled`, encoded once per test binary. Callers
/// copy it; they never write to it.
fn cached_audio_segment(titled: bool) -> &'static Path {
    static TITLED: OnceLock<PathBuf> = OnceLock::new();
    static UNTITLED: OnceLock<PathBuf> = OnceLock::new();
    let (cell, name) = if titled {
        (&TITLED, "audio-titled.mp4")
    } else {
        (&UNTITLED, "audio-untitled.mp4")
    };
    cell.get_or_init(|| {
        let path = fixture_dir().join(name);
        make_audio_segment(&path, titled);
        path
    })
}

/// Generates a 2 s mp4 with a video track and three audio tracks. When
/// `titled` is true the audio tracks are titled Mix, Game and Mic.
fn make_audio_segment(out: &Path, titled: bool) {
    let names = ["Mix", "Game", "Mic"];
    let audio: Vec<AudioSpec> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let hz = 440.0 + 110.0 * i as f32;
            if titled {
                AudioSpec::titled(hz, name)
            } else {
                AudioSpec::sine(hz)
            }
        })
        .collect();
    let spec = MediaSpec::h264_aac(2.0)
        .with_video(160, 120, 30)
        .with_audio(audio);
    clipcheck_testkit::write_mp4(out, &spec).expect("audio segment generation failed");
}

/// Titles of each audio stream in `file`, in order: `tags.title`, then
/// `tags.name`, else empty.
fn audio_titles(file: &Path) -> Vec<String> {
    clipcheck_mp4::reader::read_movie(file)
        .unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
        .tracks
        .iter()
        .filter(|t| t.is_audio())
        .map(|t| t.title.clone().unwrap_or_default())
        .collect()
}

#[test]
fn audio_titles_survive_session_concat() {
    require_media!();
    let dir = tempfile::tempdir().expect("tempdir");
    let segs: Vec<PathBuf> = (0..2)
        .map(|i| {
            let s = dir.path().join(format!("seg-{i}.mp4"));
            fs::copy(cached_audio_segment(true), &s).expect("copy audio fixture");
            s
        })
        .collect();

    let stage = dir.path().join("stage");
    let out = dir.path().join("session.mp4");
    let mut asm = SessionAssembler::new(stage).expect("new");
    for s in &segs {
        asm.add_segment(s).expect("add_segment");
    }
    asm.finalize(&out).expect("finalize");

    let titles = audio_titles(&out);
    assert_eq!(
        titles.len(),
        3,
        "expected 3 audio streams, got {}: {titles:?}",
        titles.len()
    );
    assert_eq!(titles, vec!["Mix", "Game", "Mic"]);
}

#[test]
fn audio_titles_absent_session_concat_has_empty_titles() {
    require_media!();
    let dir = tempfile::tempdir().expect("tempdir");
    let segs: Vec<PathBuf> = (0..2)
        .map(|i| {
            let s = dir.path().join(format!("seg-{i}.mp4"));
            fs::copy(cached_audio_segment(false), &s).expect("copy audio fixture");
            s
        })
        .collect();

    let stage = dir.path().join("stage");
    let out = dir.path().join("session.mp4");
    let mut asm = SessionAssembler::new(stage).expect("new");
    for s in &segs {
        asm.add_segment(s).expect("add_segment");
    }
    asm.finalize(&out)
        .expect("finalize must succeed without titles");

    let titles = audio_titles(&out);
    assert_eq!(
        titles.len(),
        3,
        "expected 3 audio streams, got {}: {titles:?}",
        titles.len()
    );
    assert!(
        titles.iter().all(|t| t.is_empty()),
        "expected empty titles, got {titles:?}"
    );
}
