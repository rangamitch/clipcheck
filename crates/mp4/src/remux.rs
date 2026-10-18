//! Concatenate / trim / track-select remux into a flat faststart MP4 (worker R2, D58).
//! Contract: docs/impl/ffmpeg-removal.md, R2.

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::fmp4::{bx, full};
use crate::reader::{read_movie, Movie, TrackKind};
use crate::Mp4Error;

/// Tolerance when comparing times on the concatenated timeline, seconds.
const EPS: f64 = 1e-6;
/// Longest span of one track's samples inside one chunk, seconds.
const MAX_CHUNK_SECS: f64 = 0.5;
/// Size of the output write buffer.
const BUF_BYTES: usize = 1 << 20;
/// Identity transformation matrix.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
/// Packed ISO 639-2 language code "und".
const LANG_UND: u16 =
    (((b'u' - 0x60) as u16) << 10) | (((b'n' - 0x60) as u16) << 5) | ((b'd' - 0x60) as u16);

/// Which tracks to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackSelect {
    /// Every track.
    All,
    /// Only these indices into the first input's track list.
    Only(Vec<usize>),
}

/// Remux settings.
#[derive(Debug, Clone, PartialEq)]
pub struct RemuxOptions {
    /// Start on the concatenated timeline, seconds (video snaps back to a keyframe).
    pub start_secs: f64,
    /// Exclusive end on the concatenated timeline, seconds.
    pub end_secs: Option<f64>,
    /// Tracks to keep.
    pub tracks: TrackSelect,
    /// Write `moov` before `mdat`.
    pub faststart: bool,
}

impl Default for RemuxOptions {
    fn default() -> Self {
        Self {
            start_secs: 0.0,
            end_secs: None,
            tracks: TrackSelect::All,
            faststart: true,
        }
    }
}

/// Result of a remux.
#[derive(Debug, Clone, PartialEq)]
pub struct RemuxReport {
    /// Output size in bytes.
    pub bytes: u64,
    /// Output duration in seconds.
    pub duration_secs: f64,
    /// Where the output really starts on the input timeline (keyframe-snapped).
    pub actual_start_secs: f64,
}

/// Concatenates `inputs` (same track layout) into the flat MP4 `out`, cut by `opts`.
pub fn concat_remux(
    inputs: &[PathBuf],
    out: &Path,
    opts: &RemuxOptions,
) -> Result<RemuxReport, Mp4Error> {
    if inputs.is_empty() {
        return Err(cfg("no input files"));
    }
    if inputs.iter().any(|p| p == out) {
        return Err(cfg("the output file is also an input"));
    }
    let part = part_path(out);
    match write_remux(inputs, &part, opts) {
        Ok(report) => {
            if let Err(e) = std::fs::rename(&part, out) {
                let _ = std::fs::remove_file(&part);
                return Err(Mp4Error::Io(e));
            }
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn cfg(msg: &str) -> Mp4Error {
    Mp4Error::InvalidConfig(msg.to_string())
}

fn bad(msg: &str) -> Mp4Error {
    Mp4Error::InvalidData(msg.to_string())
}

fn too_long() -> Mp4Error {
    bad("value exceeds the 32-bit range of the box format")
}

fn part_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// One sample of an input, placed on the concatenated timeline.
struct Cand {
    input: usize,
    offset: u64,
    size: u32,
    /// Decode time, seconds on the concatenated timeline.
    dts: f64,
    /// Presentation time, seconds on the concatenated timeline.
    pts: f64,
    /// Duration in the input track's timescale.
    dur: u32,
    /// Input track timescale.
    ts: u32,
    /// Composition offset in the input track's timescale.
    cts: i32,
    keyframe: bool,
    /// 1-based index into the output track's sample descriptions.
    desc: u32,
}

/// The candidate samples of one track across all inputs, in decode order.
struct Gathered {
    descs: Vec<Vec<u8>>,
    cands: Vec<Cand>,
}

/// Where a track's cut starts and ends.
#[derive(Clone, Copy)]
struct Cut {
    is_ref: bool,
    ref_idx: usize,
    actual: f64,
    end: Option<f64>,
}

/// One output sample.
struct OutSample {
    input: usize,
    offset: u64,
    size: u32,
    /// Decode time in output ticks.
    dts: u64,
    dur: u32,
    cts: i32,
    keyframe: bool,
    desc: u32,
}

/// A run of consecutive samples of one track stored in one `mdat` chunk.
struct Chunk {
    first: usize,
    count: usize,
    desc: u32,
    /// Decode time of the first sample, seconds.
    start: f64,
}

/// One output track.
struct OutTrack {
    kind: TrackKind,
    title: Option<String>,
    ts: u32,
    descs: Vec<Vec<u8>>,
    samples: Vec<OutSample>,
    chunks: Vec<Chunk>,
    chunk_offsets: Vec<u64>,
}

fn write_remux(
    inputs: &[PathBuf],
    part: &Path,
    opts: &RemuxOptions,
) -> Result<RemuxReport, Mp4Error> {
    if !opts.start_secs.is_finite() {
        return Err(cfg("start_secs must be finite"));
    }
    let start = opts.start_secs.max(0.0);
    let end = opts.end_secs;
    if end.is_some_and(|e| !e.is_finite() || e <= start) {
        return Err(cfg("end_secs must be finite and after start_secs"));
    }

    let movies = inputs
        .iter()
        .map(|p| read_movie(p.as_path()))
        .collect::<Result<Vec<_>, _>>()?;
    let track_count = check_layout(&movies)?;
    let selected = select_tracks(&opts.tracks, track_count)?;
    let ref_pos = selected
        .iter()
        .position(|&i| movies[0].tracks[i].is_video())
        .unwrap_or(0);
    let offsets = input_offsets(&movies);

    let (ref_idx, actual) = {
        let reference = gather(&movies, &offsets, selected[ref_pos])?;
        find_start(&reference.cands, start)?
    };

    let mut tracks = Vec::with_capacity(selected.len());
    for (pos, &src) in selected.iter().enumerate() {
        let cut = Cut {
            is_ref: pos == ref_pos,
            ref_idx,
            actual,
            end,
        };
        let track = build_track(&movies, &offsets, src, cut)?;
        if cut.is_ref && track.samples.is_empty() {
            return Err(cfg("no samples in the requested range"));
        }
        tracks.push(track);
    }

    let duration_secs = match tracks.get(ref_pos) {
        Some(r) => total_ticks(r) as f64 / f64::from(r.ts),
        None => 0.0,
    };
    let order = assign_chunks(&mut tracks);
    let (total_payload, max_rel) = assign_relative_offsets(&mut tracks, &order);

    let ftyp = ftyp_box();
    let header_len: u64 = if total_payload.saturating_add(8) > u64::from(u32::MAX) {
        16
    } else {
        8
    };
    let fixed = ftyp.len() as u64 + header_len;
    let data_start = |moov_len: u64| {
        if opts.faststart {
            fixed + moov_len
        } else {
            fixed
        }
    };

    let len32 = build_moov(&tracks, false)?.len() as u64;
    let len64 = build_moov(&tracks, true)?.len() as u64;
    let co64 = data_start(len32) + max_rel > u64::from(u32::MAX);
    let moov_len = if co64 { len64 } else { len32 };
    let base = data_start(moov_len);
    for tr in &mut tracks {
        for off in &mut tr.chunk_offsets {
            *off += base;
        }
    }
    let moov = build_moov(&tracks, co64)?;
    if moov.len() as u64 != moov_len {
        return Err(bad("moov size changed between layout passes"));
    }

    let mut files = inputs
        .iter()
        .map(|p| File::open(p.as_path()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut w = BufWriter::with_capacity(BUF_BYTES, File::create(part)?);
    w.write_all(&ftyp)?;
    if opts.faststart {
        w.write_all(&moov)?;
    }
    write_mdat_header(&mut w, total_payload, header_len)?;
    let mut buf = Vec::new();
    for &(t, ci) in &order {
        let tr = &tracks[t];
        for s in chunk_samples(tr, ci) {
            let file = files
                .get_mut(s.input)
                .ok_or_else(|| bad("sample refers to a missing input"))?;
            read_sample(file, s, &mut buf)?;
            w.write_all(&buf)?;
        }
    }
    if !opts.faststart {
        w.write_all(&moov)?;
    }
    w.flush()?;
    drop(w);

    let bytes = ftyp.len() as u64 + moov.len() as u64 + header_len + total_payload;
    Ok(RemuxReport {
        bytes,
        duration_secs,
        actual_start_secs: actual,
    })
}

/// Signature of a track list: (kind tag, codec or handler), in order.
fn layout_signature(m: &Movie) -> Vec<(u8, String)> {
    m.tracks
        .iter()
        .map(|t| match &t.kind {
            TrackKind::Video { codec, .. } => (0u8, codec.clone()),
            TrackKind::Audio { codec, .. } => (1u8, codec.clone()),
            TrackKind::Other { handler } => (2u8, handler.clone()),
        })
        .collect()
}

/// Checks that every input has the first input's track layout; returns the track count.
fn check_layout(movies: &[Movie]) -> Result<usize, Mp4Error> {
    let first = layout_signature(&movies[0]);
    if first.is_empty() {
        return Err(bad("input has no tracks"));
    }
    if movies[1..].iter().any(|m| layout_signature(m) != first) {
        return Err(bad("track layout differs between inputs"));
    }
    Ok(first.len())
}

/// Resolves a selection to sorted, unique track indices.
fn select_tracks(sel: &TrackSelect, count: usize) -> Result<Vec<usize>, Mp4Error> {
    match sel {
        TrackSelect::All => Ok((0..count).collect()),
        TrackSelect::Only(list) => {
            if list.is_empty() {
                return Err(cfg("no tracks selected"));
            }
            if list.iter().any(|&i| i >= count) {
                return Err(cfg("track index out of range"));
            }
            let mut v = list.clone();
            v.sort_unstable();
            v.dedup();
            Ok(v)
        }
    }
}

/// Duration of one input: its first video track, else its longest track.
fn input_duration(m: &Movie) -> f64 {
    match m.tracks.iter().find(|t| t.is_video()) {
        Some(t) => t.duration_secs(),
        None => m
            .tracks
            .iter()
            .map(|t| t.duration_secs())
            .fold(0.0, f64::max),
    }
}

/// Start of each input on the concatenated timeline, seconds.
fn input_offsets(movies: &[Movie]) -> Vec<f64> {
    let mut acc = 0.0;
    movies
        .iter()
        .map(|m| {
            let at = acc;
            acc += input_duration(m);
            at
        })
        .collect()
}

/// Collects every sample of track `src` from every input, in input order.
fn gather(movies: &[Movie], offsets: &[f64], src: usize) -> Result<Gathered, Mp4Error> {
    let mut descs: Vec<Vec<u8>> = Vec::new();
    let mut cands = Vec::new();
    for (k, (movie, &offset)) in movies.iter().zip(offsets).enumerate() {
        let track = movie
            .tracks
            .get(src)
            .ok_or_else(|| bad("track layout differs between inputs"))?;
        if track.timescale == 0 {
            return Err(bad("track timescale is zero"));
        }
        let mut map = Vec::with_capacity(track.sample_entries.len());
        for entry in &track.sample_entries {
            let idx = match descs.iter().position(|d| d == entry) {
                Some(i) => i,
                None => {
                    descs.push(entry.clone());
                    descs.len() - 1
                }
            };
            map.push(u32::try_from(idx + 1).map_err(|_| bad("too many sample descriptions"))?);
        }
        let ts = track.timescale;
        let ts_f = f64::from(ts);
        for s in &track.samples {
            let desc = usize::try_from(s.entry)
                .ok()
                .and_then(|e| e.checked_sub(1))
                .and_then(|e| map.get(e))
                .copied()
                .ok_or_else(|| bad("sample refers to a missing sample description"))?;
            let rel = s
                .dts
                .checked_sub(track.edit_media_time)
                .ok_or_else(|| bad("edit list offset out of range"))?;
            let dts = offset + rel as f64 / ts_f;
            let pts = dts + f64::from(s.cts_offset) / ts_f;
            cands.push(Cand {
                input: k,
                offset: s.offset,
                size: s.size,
                dts,
                pts,
                dur: s.duration,
                ts,
                cts: s.cts_offset,
                keyframe: s.keyframe,
                desc,
            });
        }
    }
    Ok(Gathered { descs, cands })
}

/// The reference cut: the last keyframe with pts <= start (else the first sample).
/// Returns its index and its pts (never negative).
fn find_start(cands: &[Cand], start: f64) -> Result<(usize, f64), Mp4Error> {
    let first = cands
        .first()
        .ok_or_else(|| cfg("the reference track has no samples"))?;
    let found = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.keyframe && c.pts <= start + EPS)
        .max_by(|a, b| a.1.pts.total_cmp(&b.1.pts));
    let (idx, pts) = match found {
        Some((i, c)) => (i, c.pts),
        None => (0, first.pts),
    };
    Ok((idx, pts.max(0.0)))
}

/// Builds one output track: cut, time-base, duration and sample-table rows.
fn build_track(
    movies: &[Movie],
    offsets: &[f64],
    src: usize,
    cut: Cut,
) -> Result<OutTrack, Mp4Error> {
    let g = gather(movies, offsets, src)?;
    let first = &movies[0].tracks[src];
    let from = if cut.is_ref {
        cut.ref_idx
    } else {
        g.cands
            .iter()
            .position(|c| c.pts >= cut.actual - EPS)
            .unwrap_or(g.cands.len())
    };
    let kept: Vec<&Cand> = g
        .cands
        .iter()
        .skip(from)
        .filter(|c| c.pts >= cut.actual - EPS && !cut.end.is_some_and(|e| c.pts >= e))
        .collect();

    let placed = place(&kept, first.timescale, first.is_video())?;
    let mut samples = Vec::with_capacity(placed.len());
    for (ki, dts, dur) in placed {
        let c = kept[ki];
        samples.push(OutSample {
            input: c.input,
            offset: c.offset,
            size: c.size,
            dts,
            dur,
            cts: scale_signed(c.cts, c.ts, first.timescale)?,
            keyframe: c.keyframe,
            desc: c.desc,
        });
    }
    Ok(OutTrack {
        kind: first.kind.clone(),
        title: first.title.clone(),
        ts: first.timescale,
        descs: g.descs,
        samples,
        chunks: Vec::new(),
        chunk_offsets: Vec::new(),
    })
}

/// Places kept samples on the output timeline. Returns (index into `kept`, decode time,
/// duration), all in output ticks. Video durations come from decode-time gaps; other
/// tracks get the audio continuity rule (gap closing, overlap clamping, drops).
fn place(kept: &[&Cand], ts_out: u32, is_video: bool) -> Result<Vec<(usize, u64, u32)>, Mp4Error> {
    let Some(base) = kept.first().map(|c| c.dts) else {
        return Ok(Vec::new());
    };
    let scale = f64::from(ts_out);
    let ticks: Vec<i64> = kept
        .iter()
        .map(|c| ((c.dts - base) * scale).round() as i64)
        .collect();

    if is_video {
        let mut out = Vec::with_capacity(kept.len());
        for (i, c) in kept.iter().enumerate() {
            let dur = match ticks.get(i + 1) {
                Some(&next) => u32::try_from((next - ticks[i]).max(0)).map_err(|_| too_long())?,
                None => {
                    let last = scale_ticks(c.dur, c.ts, ts_out)?;
                    u32::try_from(last).map_err(|_| too_long())?
                }
            };
            out.push((i, ticks[i].max(0) as u64, dur));
        }
        return Ok(out);
    }

    // (kept index, decode time, duration) in signed ticks.
    let mut placed: Vec<(usize, i64, i64)> = Vec::with_capacity(kept.len());
    let mut running: i64 = 0;
    for (i, c) in kept.iter().enumerate() {
        let d =
            i64::from(u32::try_from(scale_ticks(c.dur, c.ts, ts_out)?).map_err(|_| too_long())?);
        let s = ticks[i];
        if placed.is_empty() {
            placed.push((i, 0, d));
            running = d;
            continue;
        }
        // Starts more than half a sample before the running end: drop.
        if 2 * s < 2 * running - d {
            continue;
        }
        // Gap of more than one sample: the previous sample absorbs it.
        if s > running + d {
            if let Some(prev) = placed.last_mut() {
                prev.2 += s - running;
            }
            running = s;
        }
        placed.push((i, running, d));
        running += d;
    }
    placed
        .into_iter()
        .map(|(i, dts, dur)| {
            Ok((
                i,
                u64::try_from(dts).map_err(|_| bad("negative decode time"))?,
                u32::try_from(dur).map_err(|_| too_long())?,
            ))
        })
        .collect()
}

/// Rescales a duration from `from` to `to` ticks, rounding to nearest.
fn scale_ticks(v: u32, from: u32, to: u32) -> Result<u64, Mp4Error> {
    if from == 0 {
        return Err(bad("zero timescale"));
    }
    let num = u64::from(v) * u64::from(to);
    Ok((num + u64::from(from) / 2) / u64::from(from))
}

/// Rescales a composition offset from `from` to `to` ticks, rounding to nearest.
fn scale_signed(v: i32, from: u32, to: u32) -> Result<i32, Mp4Error> {
    if from == 0 {
        return Err(bad("zero timescale"));
    }
    let x = (f64::from(v) * f64::from(to) / f64::from(from)).round() as i64;
    i32::try_from(x).map_err(|_| bad("composition offset out of range"))
}

fn total_ticks(tr: &OutTrack) -> u64 {
    tr.samples
        .iter()
        .fold(0u64, |acc, s| acc.saturating_add(u64::from(s.dur)))
}

fn track_ms(tr: &OutTrack) -> u64 {
    let ts = u64::from(tr.ts);
    if ts == 0 {
        return 0;
    }
    total_ticks(tr).saturating_mul(1000).saturating_add(ts / 2) / ts
}

/// Walks all tracks in time order and splits them into chunks. Returns the global
/// chunk order as (track, chunk index) pairs; sizes the per-track offset tables.
fn assign_chunks(tracks: &mut [OutTrack]) -> Vec<(usize, usize)> {
    let mut ptr = vec![0usize; tracks.len()];
    let mut order = Vec::new();
    let mut last: Option<usize> = None;
    loop {
        let mut next: Option<(usize, f64)> = None;
        for (t, tr) in tracks.iter().enumerate() {
            if let Some(s) = tr.samples.get(ptr[t]) {
                let time = s.dts as f64 / f64::from(tr.ts);
                match next {
                    Some((_, best)) if best <= time => {}
                    _ => next = Some((t, time)),
                }
            }
        }
        let Some((t, time)) = next else {
            break;
        };
        let tr = &mut tracks[t];
        let desc = tr.samples[ptr[t]].desc;
        let extend = last == Some(t)
            && tr
                .chunks
                .last()
                .is_some_and(|ch| ch.desc == desc && time - ch.start <= MAX_CHUNK_SECS);
        if !extend {
            tr.chunks.push(Chunk {
                first: ptr[t],
                count: 0,
                desc,
                start: time,
            });
            // One entry per chunk: the writer emits a whole chunk per entry.
            order.push((t, tr.chunks.len().saturating_sub(1)));
        }
        let ci = tr.chunks.len().saturating_sub(1);
        if let Some(ch) = tr.chunks.get_mut(ci) {
            ch.count += 1;
        }
        ptr[t] += 1;
        last = Some(t);
    }
    for tr in tracks.iter_mut() {
        tr.chunk_offsets = vec![0; tr.chunks.len()];
    }
    order
}

/// Sets each chunk's offset relative to the start of the mdat payload.
/// Returns (total payload bytes, largest relative chunk offset).
fn assign_relative_offsets(tracks: &mut [OutTrack], order: &[(usize, usize)]) -> (u64, u64) {
    let mut pos: u64 = 0;
    let mut max_rel: u64 = 0;
    for &(t, ci) in order {
        let tr = &mut tracks[t];
        let bytes = chunk_samples(tr, ci)
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(u64::from(s.size)));
        if let Some(slot) = tr.chunk_offsets.get_mut(ci) {
            *slot = pos;
        }
        max_rel = max_rel.max(pos);
        pos = pos.saturating_add(bytes);
    }
    (pos, max_rel)
}

/// The samples of chunk `ci` of a track (empty if the index is invalid).
fn chunk_samples(tr: &OutTrack, ci: usize) -> &[OutSample] {
    tr.chunks
        .get(ci)
        .and_then(|ch| tr.samples.get(ch.first..ch.first.saturating_add(ch.count)))
        .unwrap_or(&[])
}

fn read_sample(file: &mut File, s: &OutSample, buf: &mut Vec<u8>) -> Result<(), Mp4Error> {
    file.seek(SeekFrom::Start(s.offset))?;
    buf.clear();
    buf.resize(s.size as usize, 0);
    file.read_exact(buf.as_mut_slice()).map_err(|e| {
        if e.kind() == ErrorKind::UnexpectedEof {
            bad("sample extends past the end of its input")
        } else {
            Mp4Error::Io(e)
        }
    })
}

fn write_mdat_header<W: Write>(w: &mut W, payload: u64, header_len: u64) -> std::io::Result<()> {
    if header_len == 8 {
        let size = u32::try_from(payload.saturating_add(8)).unwrap_or(u32::MAX);
        w.write_all(&size.to_be_bytes())?;
        w.write_all(b"mdat")
    } else {
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&payload.saturating_add(16).to_be_bytes())
    }
}

fn ftyp_box() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"isom");
    p.extend_from_slice(&0x200u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
        p.extend_from_slice(brand);
    }
    bx(b"ftyp", &p)
}

fn count(n: usize) -> Result<u32, Mp4Error> {
    u32::try_from(n).map_err(|_| bad("too many table entries"))
}

/// Big-endian bytes of a list of u32 values.
fn u32s(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn fourcc(s: &str) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (o, b) in out.iter_mut().zip(s.bytes()) {
        *o = b;
    }
    out
}

fn build_moov(tracks: &[OutTrack], co64: bool) -> Result<Vec<u8>, Mp4Error> {
    let mut longest_ms = 0u64;
    let mut body = Vec::new();
    // Traks are written in input order, so physical trak order matches track ids (Mix = 2 is the
    // first audio trak). Discord embeds play the first audio trak only.
    for (i, tr) in tracks.iter().enumerate() {
        let id = count(i + 1)?;
        longest_ms = longest_ms.max(track_ms(tr));
        body.extend_from_slice(&trak(tr, id, co64)?);
    }
    let movie_ms = u32::try_from(longest_ms).map_err(|_| too_long())?;
    let mut moov_body = mvhd(movie_ms, count(tracks.len() + 1)?);
    moov_body.extend_from_slice(&body);
    Ok(bx(b"moov", &moov_body))
}

/// `mvhd` (version 0): timescale 1000, the given duration and next track id.
fn mvhd(duration_ms: u32, next_track_id: u32) -> Vec<u8> {
    let mut p = u32s(&[0, 0, 1000, duration_ms, 0x0001_0000]);
    p.extend_from_slice(&0x0100u16.to_be_bytes());
    p.extend_from_slice(&[0u8; 10]);
    p.extend_from_slice(&u32s(&MATRIX));
    p.extend_from_slice(&[0u8; 24]);
    p.extend_from_slice(&next_track_id.to_be_bytes());
    full(b"mvhd", 0, 0, &p)
}

/// `tkhd` (version 0, flags 3).
fn tkhd(
    id: u32,
    dims: Option<(u32, u32)>,
    duration_ms: u32,
    alternate: u16,
    volume: u16,
) -> Vec<u8> {
    let (w, h) = dims.unwrap_or((0, 0));
    let mut p = u32s(&[0, 0, id, 0, duration_ms, 0, 0]);
    p.extend_from_slice(&0u16.to_be_bytes()); // layer
    p.extend_from_slice(&alternate.to_be_bytes());
    p.extend_from_slice(&volume.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&u32s(&MATRIX));
    p.extend_from_slice(&u32s(&[w << 16, h << 16]));
    full(b"tkhd", 0, 3, &p)
}

/// `mdhd` (version 0).
fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = u32s(&[0, 0, timescale, duration]);
    p.extend_from_slice(&LANG_UND.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    full(b"mdhd", 0, 0, &p)
}

/// `hdlr` (version 0) for a handler type and a NUL-terminated name.
fn hdlr(handler: &[u8; 4], name: &str) -> Vec<u8> {
    let mut p = u32s(&[0]);
    p.extend_from_slice(handler);
    p.extend_from_slice(&u32s(&[0, 0, 0]));
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    full(b"hdlr", 0, 0, &p)
}

/// `dinf` with a self-contained `dref`.
fn dinf() -> Vec<u8> {
    let url = full(b"url ", 0, 1, &[]);
    let dref = [u32s(&[1]), url].concat();
    bx(b"dinf", &full(b"dref", 0, 0, &dref))
}

fn trak(tr: &OutTrack, id: u32, co64: bool) -> Result<Vec<u8>, Mp4Error> {
    let total = u32::try_from(total_ticks(tr)).map_err(|_| too_long())?;
    let ms = u32::try_from(track_ms(tr)).map_err(|_| too_long())?;
    let (dims, handler, name, media_header, alternate, volume) = match &tr.kind {
        TrackKind::Video { width, height, .. } => (
            Some((*width, *height)),
            *b"vide",
            "VideoHandler",
            full(b"vmhd", 0, 1, &[0u8; 8]),
            0u16,
            0u16,
        ),
        TrackKind::Audio { .. } => (
            None,
            *b"soun",
            "SoundHandler",
            full(b"smhd", 0, 0, &[0u8; 4]),
            1u16,
            0x0100u16,
        ),
        TrackKind::Other { handler } => (
            None,
            fourcc(handler),
            "",
            full(b"nmhd", 0, 0, &[]),
            0u16,
            0u16,
        ),
    };
    let stbl_box = stbl(tr, co64)?;
    let minf = bx(b"minf", &[media_header, dinf(), stbl_box].concat());
    let mdia = bx(
        b"mdia",
        &[mdhd(tr.ts, total), hdlr(&handler, name), minf].concat(),
    );
    let mut body = vec![tkhd(id, dims, ms, alternate, volume), mdia];
    if let Some(title) = tr.title.as_deref().filter(|t| !t.is_empty()) {
        body.push(bx(b"udta", &bx(b"name", title.as_bytes())));
    }
    Ok(bx(b"trak", &body.concat()))
}

fn stbl(tr: &OutTrack, co64: bool) -> Result<Vec<u8>, Mp4Error> {
    // stsd: every distinct sample entry, verbatim.
    let mut stsd_p = u32s(&[count(tr.descs.len())?]);
    for d in &tr.descs {
        stsd_p.extend_from_slice(d);
    }
    let stsd = full(b"stsd", 0, 0, &stsd_p);

    // stts: run-length durations.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in &tr.samples {
        match runs.last_mut() {
            Some((n, d)) if *d == s.dur => *n = n.saturating_add(1),
            _ => runs.push((1, s.dur)),
        }
    }
    let mut stts_p = u32s(&[count(runs.len())?]);
    for (n, d) in &runs {
        stts_p.extend_from_slice(&u32s(&[*n, *d]));
    }
    let stts = full(b"stts", 0, 0, &stts_p);

    // ctts (version 1, signed), only when some offset is non-zero.
    let ctts = if tr.samples.iter().any(|s| s.cts != 0) {
        let mut cruns: Vec<(u32, i32)> = Vec::new();
        for s in &tr.samples {
            match cruns.last_mut() {
                Some((n, c)) if *c == s.cts => *n = n.saturating_add(1),
                _ => cruns.push((1, s.cts)),
            }
        }
        let mut p = u32s(&[count(cruns.len())?]);
        for (n, c) in &cruns {
            p.extend_from_slice(&n.to_be_bytes());
            p.extend_from_slice(&c.to_be_bytes());
        }
        full(b"ctts", 1, 0, &p)
    } else {
        Vec::new()
    };

    // stss: video only, and only when some sample is not a sync sample.
    let is_video = matches!(tr.kind, TrackKind::Video { .. });
    let stss = if is_video && tr.samples.iter().any(|s| !s.keyframe) {
        let mut list = Vec::new();
        for (i, s) in tr.samples.iter().enumerate() {
            if s.keyframe {
                list.push(count(i + 1)?);
            }
        }
        let mut p = u32s(&[count(list.len())?]);
        p.extend_from_slice(&u32s(&list));
        full(b"stss", 0, 0, &p)
    } else {
        Vec::new()
    };

    // stsc: run-length (first chunk, samples per chunk, description).
    let mut stsc_runs: Vec<(u32, u32, u32)> = Vec::new();
    for (i, ch) in tr.chunks.iter().enumerate() {
        let spc = count(ch.count)?;
        let same = matches!(stsc_runs.last(), Some(&(_, n, d)) if n == spc && d == ch.desc);
        if !same {
            stsc_runs.push((count(i + 1)?, spc, ch.desc));
        }
    }
    let mut stsc_p = u32s(&[count(stsc_runs.len())?]);
    for (a, b, c) in &stsc_runs {
        stsc_p.extend_from_slice(&u32s(&[*a, *b, *c]));
    }
    let stsc = full(b"stsc", 0, 0, &stsc_p);

    // stsz: per-sample sizes.
    let mut stsz_p = u32s(&[0, count(tr.samples.len())?]);
    for s in &tr.samples {
        stsz_p.extend_from_slice(&s.size.to_be_bytes());
    }
    let stsz = full(b"stsz", 0, 0, &stsz_p);

    // stco / co64: chunk offsets.
    let n = count(tr.chunk_offsets.len())?;
    let stco = if co64 {
        let mut p = u32s(&[n]);
        for o in &tr.chunk_offsets {
            p.extend_from_slice(&o.to_be_bytes());
        }
        full(b"co64", 0, 0, &p)
    } else {
        let mut p = u32s(&[n]);
        for o in &tr.chunk_offsets {
            // Offsets fit by construction when co64 is off (see write_remux).
            p.extend_from_slice(&u32::try_from(*o).unwrap_or(u32::MAX).to_be_bytes());
        }
        full(b"stco", 0, 0, &p)
    };

    Ok(bx(
        b"stbl",
        &[stsd, stts, ctts, stss, stsc, stsz, stco].concat(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aac::aac_lc_asc;
    use crate::fmp4::AudioTrackSpec;
    use crate::segment::{AudioPacket, SegmentConfig, SegmentMuxer, VideoPacket};

    const SPS: [u8; 30] = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01,
        0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60,
    ];
    const PPS: [u8; 10] = [0x00, 0x00, 0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
    const IDR: [u8; 10] = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x33, 0xFF];
    const NON_IDR: [u8; 8] = [0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x02, 0x03];

    fn au(key: bool) -> Vec<u8> {
        if key {
            [&SPS[..], &PPS[..], &IDR[..]].concat()
        } else {
            NON_IDR.to_vec()
        }
    }

    /// Audio pts (100 ns) of AAC frame `j` at 48 kHz.
    fn audio_pts(j: i64) -> i64 {
        j * 1024 * 10_000_000 / 48_000
    }

    /// Writes `secs` seconds of 30 fps H.264 (plus AAC when `audio`) as segments into `dir`.
    fn make_segments(dir: &Path, secs: u32, audio: bool) -> Vec<PathBuf> {
        let spec = AudioTrackSpec {
            sample_rate: 48_000,
            channels: 2,
            asc: aac_lc_asc(48_000, 2).unwrap_or_default(),
            bitrate: 128_000,
            title: String::new(),
        };
        let cfg = SegmentConfig {
            out_dir: dir.to_path_buf(),
            segment_secs: 1.0,
            fps: 30,
            codec: crate::VideoCodec::H264,
            width: 320,
            height: 240,
            audio: if audio { vec![spec] } else { Vec::new() },
        };
        let mut m = SegmentMuxer::new(cfg).unwrap();
        let audio_total = if audio {
            i64::from(secs) * 48_000 / 1024
        } else {
            0
        };
        let mut next_audio = 0i64;
        for i in 0..i64::from(secs) * 30 {
            let pts = i * 10_000_000 / 30;
            let key = i % 30 == 0;
            m.push_video(VideoPacket {
                data: au(key),
                pts,
                dts: pts,
                keyframe: key,
            })
            .unwrap();
            while next_audio < audio_total && audio_pts(next_audio) <= pts {
                let apts = audio_pts(next_audio);
                m.push_audio(
                    0,
                    AudioPacket {
                        data: vec![0xAA; 4],
                        pts: apts,
                        samples: 1024,
                    },
                )
                .unwrap();
                next_audio += 1;
            }
        }
        while next_audio < audio_total {
            m.push_audio(
                0,
                AudioPacket {
                    data: vec![0xAA; 4],
                    pts: audio_pts(next_audio),
                    samples: 1024,
                },
            )
            .unwrap();
            next_audio += 1;
        }
        m.finish().unwrap()
    }

    fn video_count(m: &Movie) -> usize {
        m.tracks
            .iter()
            .find(|t| t.is_video())
            .map_or(0, |t| t.samples.len())
    }

    fn opts(start: f64, end: Option<f64>, tracks: TrackSelect) -> RemuxOptions {
        RemuxOptions {
            start_secs: start,
            end_secs: end,
            tracks,
            faststart: true,
        }
    }

    #[test]
    fn concat_three_segments_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 3, true);
        assert_eq!(segs.len(), 3);
        let out = dir.path().join("out.mp4");
        let rep = concat_remux(&segs, &out, &RemuxOptions::default()).unwrap();
        let m = read_movie(&out).unwrap();
        assert_eq!(m.tracks.len(), 2);
        assert_eq!(video_count(&m), 90);
        assert!(m.tracks.iter().any(|t| t.is_audio()));
        assert!((m.duration_secs - 3.0).abs() < 0.05, "{}", m.duration_secs);
        assert!((rep.duration_secs - 3.0).abs() < 0.05);
        assert_eq!(rep.actual_start_secs, 0.0);
        assert_eq!(rep.bytes, std::fs::metadata(&out).unwrap().len());
        assert!(!dir.path().join("out.mp4.part").exists());
    }

    /// Regression: every chunk was written once per sample it holds, so the output of a real
    /// recording came out ~24x larger than the samples it contains.
    #[test]
    fn remux_writes_each_sample_once() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 3, true);
        let input_bytes: u64 = segs
            .iter()
            .map(|p| std::fs::metadata(p).unwrap().len())
            .sum();
        let out = dir.path().join("out.mp4");
        let rep = concat_remux(&segs, &out, &RemuxOptions::default()).unwrap();
        assert!(
            rep.bytes <= input_bytes,
            "output {} bytes exceeds the {} bytes of input",
            rep.bytes,
            input_bytes
        );
        let out_audio = dir.path().join("audio.m4a");
        let rep = concat_remux(
            &segs,
            &out_audio,
            &opts(0.0, None, TrackSelect::Only(vec![1])),
        )
        .unwrap();
        assert!(
            rep.bytes < input_bytes,
            "audio-only output is not smaller than the inputs"
        );
    }

    #[test]
    fn trim_start_snaps_back_to_keyframe() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 3, true);
        let out = dir.path().join("out.mp4");
        let rep = concat_remux(&segs, &out, &opts(1.4, None, TrackSelect::All)).unwrap();
        assert!(
            (rep.actual_start_secs - 1.0).abs() < 1e-3,
            "{}",
            rep.actual_start_secs
        );
        let m = read_movie(&out).unwrap();
        assert_eq!(video_count(&m), 60);
    }

    #[test]
    fn end_secs_excludes_later_samples() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 3, true);
        let out = dir.path().join("out.mp4");
        concat_remux(&segs, &out, &opts(0.0, Some(2.0), TrackSelect::All)).unwrap();
        let m = read_movie(&out).unwrap();
        assert_eq!(video_count(&m), 60);
    }

    #[test]
    fn only_audio_yields_one_track() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 2, true);
        let out = dir.path().join("audio.mp4");
        concat_remux(&segs, &out, &opts(0.0, None, TrackSelect::Only(vec![1]))).unwrap();
        let m = read_movie(&out).unwrap();
        assert_eq!(m.tracks.len(), 1);
        assert!(m.tracks[0].is_audio());
    }

    #[test]
    fn out_of_range_selection_is_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let segs = make_segments(&dir.path().join("src"), 1, true);
        let out = dir.path().join("out.mp4");
        let err = concat_remux(&segs, &out, &opts(0.0, None, TrackSelect::Only(vec![5])));
        assert!(matches!(err, Err(Mp4Error::InvalidConfig(_))));
        assert!(!out.exists());
    }

    #[test]
    fn mismatched_layout_is_invalid_data_and_leaves_output_alone() {
        let dir = tempfile::tempdir().unwrap();
        let av = make_segments(&dir.path().join("av"), 1, true);
        let v = make_segments(&dir.path().join("v"), 1, false);
        let out = dir.path().join("out.mp4");
        std::fs::write(&out, b"previous").unwrap();
        let mixed = vec![av[0].clone(), v[0].clone()];
        let err = concat_remux(&mixed, &out, &RemuxOptions::default());
        assert!(matches!(err, Err(Mp4Error::InvalidData(_))));
        assert_eq!(std::fs::read(&out).unwrap(), b"previous");
        assert!(!dir.path().join("out.mp4.part").exists());
    }

    #[test]
    fn empty_input_list_is_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mp4");
        let err = concat_remux(&[], &out, &RemuxOptions::default());
        assert!(matches!(err, Err(Mp4Error::InvalidConfig(_))));
    }
}
