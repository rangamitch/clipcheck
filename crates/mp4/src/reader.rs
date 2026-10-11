//! MP4/MOV reader: flat and fragmented files into tracks and sample tables (worker R1, D58).
//! Contract: docs/impl/ffmpeg-removal.md, R1.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::Mp4Error;

/// Maximum number of tracks accepted from one file.
const MAX_TRACKS: usize = 64;
/// Maximum number of samples accepted for one track.
const MAX_SAMPLES: usize = 10_000_000;
/// Maximum size of one `stsd` sample entry.
const MAX_ENTRY_BYTES: usize = 1 << 20;
/// Maximum size of a `moov` or `moof` box read into memory.
const MAX_META_BYTES: u64 = 512 << 20;

/// A parsed movie.
#[derive(Debug, Clone, PartialEq)]
pub struct Movie {
    /// Tracks in file order.
    pub tracks: Vec<Track>,
    /// Longest track duration in seconds.
    pub duration_secs: f64,
}

/// One track.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `tkhd` track id.
    pub id: u32,
    /// What the track carries.
    pub kind: TrackKind,
    /// `mdhd` timescale.
    pub timescale: u32,
    /// `trak/udta/name` title, if any.
    pub title: Option<String>,
    /// Each `stsd` entry as a full box, verbatim.
    pub sample_entries: Vec<Vec<u8>>,
    /// First non-empty edit's media_time (track timescale); 0 without an edit list.
    pub edit_media_time: i64,
    /// Samples in decode order.
    pub samples: Vec<SampleRef>,
}

/// Track type and its main properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackKind {
    /// Video track.
    Video {
        /// "h264", "hevc", "av1" or the sample entry fourcc.
        codec: String,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// Audio track.
    Audio {
        /// "aac", "opus" or the sample entry fourcc.
        codec: String,
        /// Channel count.
        channels: u16,
        /// Sample rate in Hz.
        sample_rate: u32,
    },
    /// Any other handler.
    Other {
        /// `hdlr` handler type.
        handler: String,
    },
}

/// Location and timing of one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRef {
    /// Absolute file offset of the sample bytes.
    pub offset: u64,
    /// Sample size in bytes.
    pub size: u32,
    /// Decode time in the track timescale (first sample = 0, before edit lists).
    pub dts: i64,
    /// Composition offset (pts - dts).
    pub cts_offset: i32,
    /// Duration in the track timescale.
    pub duration: u32,
    /// Sync sample.
    pub keyframe: bool,
    /// 1-based sample description index.
    pub entry: u32,
}

impl Track {
    /// Sum of sample durations in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        let total: u64 = self.samples.iter().map(|s| u64::from(s.duration)).sum();
        total as f64 / f64::from(self.timescale)
    }

    /// True for video tracks.
    pub fn is_video(&self) -> bool {
        matches!(self.kind, TrackKind::Video { .. })
    }

    /// True for audio tracks.
    pub fn is_audio(&self) -> bool {
        matches!(self.kind, TrackKind::Audio { .. })
    }
}

/// Reads the movie structure of the file at `path`.
pub fn read_movie(path: &Path) -> Result<Movie, Mp4Error> {
    let file = File::open(path)?;
    read_movie_from(BufReader::new(file))
}

/// Reads the movie structure from a seekable reader.
pub fn read_movie_from<R: Read + Seek>(mut r: R) -> Result<Movie, Mp4Error> {
    let len = r.seek(SeekFrom::End(0))?;
    let mut pending: Option<Pending> = None;
    let mut pos: u64 = 0;
    while pos < len {
        let avail = len - pos;
        let avail_us = usize::try_from(avail).unwrap_or(usize::MAX);
        let n = avail_us.min(16);
        let mut hb = [0u8; 16];
        r.seek(SeekFrom::Start(pos))?;
        r.read_exact(&mut hb[..n])?;
        if n < 8 {
            break;
        }
        let (size, typ, hlen) = match header(&hb[..n], avail_us) {
            Ok(h) => h,
            Err(_) => break,
        };
        let size = u64::try_from(size).unwrap_or(u64::MAX);
        if size > avail {
            // Truncated top-level box (file still being written): keep what is complete.
            if &typ == b"moov" {
                return Err(bad("truncated moov box"));
            }
            break;
        }
        if &typ == b"moov" || &typ == b"moof" {
            if size > MAX_META_BYTES {
                return Err(bad("moov or moof box too large"));
            }
            let usz = usize::try_from(size).unwrap_or(usize::MAX);
            let mut buf = vec![0u8; usz];
            r.seek(SeekFrom::Start(pos))?;
            r.read_exact(&mut buf)?;
            let body = buf
                .get(hlen..)
                .ok_or_else(|| bad("box smaller than its header"))?;
            if &typ == b"moov" {
                pending = Some(parse_moov(body)?);
            } else if let Some(p) = pending.as_mut() {
                parse_moof(body, pos, p)?;
            }
        }
        // Box sizes are at least 8 bytes, so this always advances. mdat payloads are skipped.
        pos += size;
    }

    let pending = pending.ok_or_else(|| bad("no moov box"))?;
    let mut tracks = pending.tracks;
    // Files written by ClipCheck place the default (first) audio track last in the moov; track
    // ids are in logical order, so sort by id to give callers the logical order (Mix first).
    tracks.sort_by_key(|t| t.id);
    for t in &mut tracks {
        t.samples.retain(|s| {
            s.offset
                .checked_add(u64::from(s.size))
                .is_some_and(|end| end <= len)
        });
    }
    let duration_secs = tracks.iter().map(Track::duration_secs).fold(0.0, f64::max);
    Ok(Movie {
        tracks,
        duration_secs,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds an `InvalidData` error.
fn bad(msg: &str) -> Mp4Error {
    Mp4Error::InvalidData(msg.to_string())
}

/// Parses a box header at the start of `b`. `avail` is the number of bytes the box may
/// occupy (used for size 0, "to end of file/parent"). Returns (size, type, header length).
/// The caller checks that `size` fits in its container.
fn header(b: &[u8], avail: usize) -> Result<(usize, [u8; 4], usize), Mp4Error> {
    let mut c = Cur::new(b);
    let size32 = c.u32()?;
    let typ = c.arr::<4>()?;
    let (size, hlen) = match size32 {
        0 => (avail, 8usize),
        1 => (usize::try_from(c.u64()?).unwrap_or(usize::MAX), 16usize),
        n => (usize::try_from(n).unwrap_or(usize::MAX), 8usize),
    };
    if size < hlen {
        return Err(bad("box smaller than its header"));
    }
    Ok((size, typ, hlen))
}

/// A child box: its type and payload (after the header).
struct BoxRef<'a> {
    typ: [u8; 4],
    body: &'a [u8],
}

/// Splits `b` into its child boxes. Every child must fit inside `b`.
fn boxes(b: &[u8]) -> Result<Vec<BoxRef<'_>>, Mp4Error> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        let rest = b.get(p..).unwrap_or(&[]);
        let (size, typ, hlen) = header(rest, rest.len())?;
        if size > rest.len() {
            return Err(bad("box size exceeds its parent"));
        }
        let body = rest
            .get(hlen..size)
            .ok_or_else(|| bad("box size exceeds its parent"))?;
        out.push(BoxRef { typ, body });
        p += size;
    }
    Ok(out)
}

/// Bounds-checked big-endian cursor over a byte slice.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, p: 0 }
    }

    fn rest(&self) -> usize {
        self.b.len().saturating_sub(self.p)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Mp4Error> {
        let b = self.b;
        let end = self
            .p
            .checked_add(n)
            .filter(|&e| e <= b.len())
            .ok_or_else(|| bad("truncated box"))?;
        let s = b.get(self.p..end).ok_or_else(|| bad("truncated box"))?;
        self.p = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], Mp4Error> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, Mp4Error> {
        Ok(self.arr::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Mp4Error> {
        Ok(u16::from_be_bytes(self.arr()?))
    }

    fn u32(&mut self) -> Result<u32, Mp4Error> {
        Ok(u32::from_be_bytes(self.arr()?))
    }

    fn i32(&mut self) -> Result<i32, Mp4Error> {
        Ok(i32::from_be_bytes(self.arr()?))
    }

    fn u64(&mut self) -> Result<u64, Mp4Error> {
        Ok(u64::from_be_bytes(self.arr()?))
    }

    fn i64(&mut self) -> Result<i64, Mp4Error> {
        Ok(i64::from_be_bytes(self.arr()?))
    }

    fn skip(&mut self, n: usize) -> Result<(), Mp4Error> {
        self.take(n).map(|_| ())
    }

    /// Version and 24-bit flags of a full box.
    fn fullbox(&mut self) -> Result<(u8, u32), Mp4Error> {
        let v = self.u8()?;
        let f = self.arr::<3>()?;
        Ok((v, u32::from_be_bytes([0, f[0], f[1], f[2]])))
    }

    /// Validates an entry count against the bytes left, before any allocation.
    fn count(&self, raw: u32, each: usize) -> Result<usize, Mp4Error> {
        let n = usize::try_from(raw).unwrap_or(usize::MAX);
        match n.checked_mul(each) {
            Some(total) if total <= self.rest() => Ok(n),
            _ => Err(bad("table entry count exceeds its box")),
        }
    }

    /// The next whole child box, verbatim (header included).
    fn sub_box(&mut self) -> Result<&'a [u8], Mp4Error> {
        let b = self.b;
        let rest = b.get(self.p..).unwrap_or(&[]);
        let (size, _, _) = header(rest, rest.len())?;
        self.take(size)
    }
}

/// Box version 0 (32-bit time fields) or 1 (64-bit time fields).
fn wide(v: u8) -> Result<bool, Mp4Error> {
    match v {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(bad("unsupported box version")),
    }
}

/// Converts a four-character code to a string (lossy for non-UTF-8 bytes).
fn fourcc(t: &[u8; 4]) -> String {
    String::from_utf8_lossy(t).into_owned()
}

/// Validates a sample count from a table header.
fn sample_count(raw: u32) -> Result<usize, Mp4Error> {
    let n = usize::try_from(raw).unwrap_or(usize::MAX);
    if n > MAX_SAMPLES {
        return Err(bad("track has too many samples"));
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// moov
// ---------------------------------------------------------------------------

/// Per-track defaults from `mvex/trex`.
#[derive(Debug, Clone, Copy)]
struct Trex {
    sdi: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

/// Movie state while parsing: tracks from `moov` plus the `trex` defaults.
struct Pending {
    tracks: Vec<Track>,
    trex: Vec<(u32, Trex)>,
}

fn parse_moov(body: &[u8]) -> Result<Pending, Mp4Error> {
    let mut p = Pending {
        tracks: Vec::new(),
        trex: Vec::new(),
    };
    for b in boxes(body)? {
        match &b.typ {
            b"trak" => {
                if p.tracks.len() >= MAX_TRACKS {
                    return Err(bad("too many tracks"));
                }
                p.tracks.push(parse_trak(b.body)?);
            }
            b"mvex" => {
                for t in boxes(b.body)? {
                    if &t.typ == b"trex" {
                        p.trex.push(parse_trex(t.body)?);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(p)
}

fn parse_trex(body: &[u8]) -> Result<(u32, Trex), Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let id = c.u32()?;
    let sdi = c.u32()?;
    let duration = c.u32()?;
    let size = c.u32()?;
    let flags = c.u32()?;
    Ok((
        id,
        Trex {
            sdi,
            duration,
            size,
            flags,
        },
    ))
}

fn parse_trak(body: &[u8]) -> Result<Track, Mp4Error> {
    let mut id = 0u32;
    let mut timescale = 0u32;
    let mut handler = [0u8; 4];
    let mut stbl: Option<&[u8]> = None;
    let mut title: Option<String> = None;
    let mut edit_media_time = 0i64;
    for b in boxes(body)? {
        match &b.typ {
            b"tkhd" => id = parse_tkhd(b.body)?,
            b"edts" => {
                for e in boxes(b.body)? {
                    if &e.typ == b"elst" {
                        if let Some(mt) = parse_elst(e.body)? {
                            edit_media_time = mt;
                        }
                    }
                }
            }
            b"udta" => {
                for u in boxes(b.body)? {
                    if &u.typ == b"name" {
                        title = parse_name(u.body);
                    }
                }
            }
            b"mdia" => {
                for m in boxes(b.body)? {
                    match &m.typ {
                        b"mdhd" => timescale = parse_mdhd(m.body)?,
                        b"hdlr" => handler = parse_hdlr(m.body)?,
                        b"minf" => {
                            for mi in boxes(m.body)? {
                                if &mi.typ == b"stbl" {
                                    stbl = Some(mi.body);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let (sample_entries, samples) = match stbl {
        Some(s) => parse_stbl(s)?,
        None => (Vec::new(), Vec::new()),
    };
    let kind = track_kind(&handler, sample_entries.first().map(|e| e.as_slice()))?;
    Ok(Track {
        id,
        kind,
        timescale,
        title,
        sample_entries,
        edit_media_time,
        samples,
    })
}

fn parse_tkhd(body: &[u8]) -> Result<u32, Mp4Error> {
    let mut c = Cur::new(body);
    let (v, _) = c.fullbox()?;
    c.skip(if wide(v)? { 16 } else { 8 })?;
    c.u32()
}

fn parse_mdhd(body: &[u8]) -> Result<u32, Mp4Error> {
    let mut c = Cur::new(body);
    let (v, _) = c.fullbox()?;
    c.skip(if wide(v)? { 16 } else { 8 })?;
    c.u32()
}

fn parse_hdlr(body: &[u8]) -> Result<[u8; 4], Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    c.skip(4)?; // pre_defined
    c.arr::<4>()
}

/// First non-empty edit's media_time, if any.
fn parse_elst(body: &[u8]) -> Result<Option<i64>, Mp4Error> {
    let mut c = Cur::new(body);
    let (v, _) = c.fullbox()?;
    let w = wide(v)?;
    let entry_size = if w { 20 } else { 12 };
    let raw = c.u32()?;
    let n = c.count(raw, entry_size)?;
    for _ in 0..n {
        let mt = if w {
            c.skip(8)?; // segment_duration
            c.i64()?
        } else {
            c.skip(4)?; // segment_duration
            i64::from(c.i32()?)
        };
        c.skip(4)?; // media_rate integer and fraction
        if mt != -1 {
            return Ok(Some(mt));
        }
    }
    Ok(None)
}

/// `udta/name` payload: UTF-8 text with trailing NULs removed; empty means no title.
fn parse_name(body: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(body);
    let t = s.trim_end_matches('\0');
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Track kind from the handler type and the first sample entry.
fn track_kind(handler: &[u8; 4], first: Option<&[u8]>) -> Result<TrackKind, Mp4Error> {
    match handler {
        b"vide" => {
            let e = first.ok_or_else(|| bad("video track without sample entry"))?;
            let (typ, body) = entry_body(e)?;
            // VisualSampleEntry: 6 reserved, 2 data_reference_index, 16 pre_defined/reserved.
            let mut c = Cur::new(body);
            c.skip(24)?;
            let width = u32::from(c.u16()?);
            let height = u32::from(c.u16()?);
            let codec = match &typ {
                b"avc1" | b"avc3" => "h264".to_string(),
                b"hvc1" | b"hev1" => "hevc".to_string(),
                b"av01" => "av1".to_string(),
                other => fourcc(other),
            };
            Ok(TrackKind::Video {
                codec,
                width,
                height,
            })
        }
        b"soun" => {
            let e = first.ok_or_else(|| bad("audio track without sample entry"))?;
            let (typ, body) = entry_body(e)?;
            // AudioSampleEntry: 6 reserved, 2 data_reference_index, 8 reserved, then fields.
            let mut c = Cur::new(body);
            c.skip(16)?;
            let channels = c.u16()?;
            c.skip(6)?; // samplesize, pre_defined, reserved
            let sample_rate = c.u32()? >> 16;
            let codec = match &typ {
                b"mp4a" => "aac".to_string(),
                b"Opus" => "opus".to_string(),
                other => fourcc(other),
            };
            Ok(TrackKind::Audio {
                codec,
                channels,
                sample_rate,
            })
        }
        other => Ok(TrackKind::Other {
            handler: fourcc(other),
        }),
    }
}

/// Type and payload of one complete sample-entry box.
fn entry_body(e: &[u8]) -> Result<([u8; 4], &[u8]), Mp4Error> {
    let (size, typ, hlen) = header(e, e.len())?;
    let body = e
        .get(hlen..size)
        .ok_or_else(|| bad("bad sample entry size"))?;
    Ok((typ, body))
}

// ---------------------------------------------------------------------------
// Sample tables (flat files)
// ---------------------------------------------------------------------------

/// Sample sizes: one uniform size (`stsz` with size != 0) or a table.
struct Sizes {
    uniform: u32,
    count: usize,
    table: Vec<u32>,
}

impl Sizes {
    fn at(&self, i: usize) -> u32 {
        if self.uniform != 0 {
            self.uniform
        } else {
            self.table.get(i).copied().unwrap_or(0)
        }
    }
}

/// The sample tables of one `stbl`.
struct Tables {
    /// (sample count, sample delta).
    stts: Vec<(u32, u32)>,
    /// (sample count, composition offset).
    ctts: Vec<(u32, i32)>,
    /// Sync sample numbers (1-based); `None` means every sample is sync.
    stss: Option<Vec<u32>>,
    /// (first chunk (1-based), samples per chunk, sample description index).
    stsc: Vec<(u32, u32, u32)>,
    sizes: Sizes,
    /// Absolute chunk offsets.
    offsets: Vec<u64>,
}

fn parse_stbl(body: &[u8]) -> Result<(Vec<Vec<u8>>, Vec<SampleRef>), Mp4Error> {
    let kids = boxes(body)?;
    let get = |t: &[u8; 4]| kids.iter().find(|k| &k.typ == t).map(|k| k.body);

    let entries = match get(b"stsd") {
        Some(b) => parse_stsd(b)?,
        None => Vec::new(),
    };
    let stts = get(b"stts")
        .map(parse_stts)
        .transpose()?
        .unwrap_or_default();
    let ctts = get(b"ctts")
        .map(parse_ctts)
        .transpose()?
        .unwrap_or_default();
    let stss = get(b"stss").map(parse_stss).transpose()?;
    let stsc = get(b"stsc")
        .map(parse_stsc)
        .transpose()?
        .unwrap_or_default();
    let sizes = if let Some(b) = get(b"stsz") {
        parse_stsz(b)?
    } else if let Some(b) = get(b"stz2") {
        parse_stz2(b)?
    } else {
        Sizes {
            uniform: 0,
            count: 0,
            table: Vec::new(),
        }
    };
    let offsets = if let Some(b) = get(b"co64") {
        parse_chunk_offsets(b, true)?
    } else if let Some(b) = get(b"stco") {
        parse_chunk_offsets(b, false)?
    } else {
        Vec::new()
    };
    let tables = Tables {
        stts,
        ctts,
        stss,
        stsc,
        sizes,
        offsets,
    };
    let samples = tables.samples()?;
    Ok((entries, samples))
}

impl Tables {
    fn samples(&self) -> Result<Vec<SampleRef>, Mp4Error> {
        let n = self.sizes.count;
        let mut out = Vec::with_capacity(n.min(1 << 16));
        let mut durs = self
            .stts
            .iter()
            .flat_map(|&(c, d)| std::iter::repeat_n(d, c as usize))
            .chain(std::iter::repeat(0));
        let mut ctss = self
            .ctts
            .iter()
            .flat_map(|&(c, o)| std::iter::repeat_n(o, c as usize))
            .chain(std::iter::repeat(0));
        let mut k = 0usize;
        let mut sample = 0usize;
        let mut dts = 0i64;
        for (ci, &chunk_offset) in self.offsets.iter().enumerate() {
            if sample >= n {
                break;
            }
            let chunk_no = u32::try_from(ci + 1).unwrap_or(u32::MAX);
            while self.stsc.get(k + 1).is_some_and(|e| e.0 <= chunk_no) {
                k += 1;
            }
            let &(first, per_chunk, entry) = self
                .stsc
                .get(k)
                .ok_or_else(|| bad("sample-to-chunk table is empty"))?;
            if first > chunk_no {
                return Err(bad("sample-to-chunk table does not cover chunk"));
            }
            let mut off = chunk_offset;
            for _ in 0..per_chunk {
                if sample >= n {
                    break;
                }
                let size = self.sizes.at(sample);
                let duration = durs.next().unwrap_or(0);
                let cts_offset = ctss.next().unwrap_or(0);
                let keyframe = match &self.stss {
                    Some(s) => s
                        .binary_search(&u32::try_from(sample + 1).unwrap_or(u32::MAX))
                        .is_ok(),
                    None => true,
                };
                out.push(SampleRef {
                    offset: off,
                    size,
                    dts,
                    cts_offset,
                    duration,
                    keyframe,
                    entry,
                });
                off = off
                    .checked_add(u64::from(size))
                    .ok_or_else(|| bad("sample offset overflows"))?;
                dts = dts
                    .checked_add(i64::from(duration))
                    .ok_or_else(|| bad("decode time overflows"))?;
                sample += 1;
            }
        }
        if sample < n {
            return Err(bad("sample tables cover fewer samples than stsz"));
        }
        Ok(out)
    }
}

fn parse_stsd(body: &[u8]) -> Result<Vec<Vec<u8>>, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, 8)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let e = c.sub_box()?;
        if e.len() > MAX_ENTRY_BYTES {
            return Err(bad("sample entry too large"));
        }
        out.push(e.to_vec());
    }
    Ok(out)
}

fn parse_stts(body: &[u8]) -> Result<Vec<(u32, u32)>, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, 8)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let count = c.u32()?;
        let delta = c.u32()?;
        out.push((count, delta));
    }
    Ok(out)
}

fn parse_ctts(body: &[u8]) -> Result<Vec<(u32, i32)>, Mp4Error> {
    let mut c = Cur::new(body);
    let (v, _) = c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, 8)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let count = c.u32()?;
        let offset = if v == 0 {
            i32::try_from(c.u32()?).unwrap_or(i32::MAX)
        } else {
            c.i32()?
        };
        out.push((count, offset));
    }
    Ok(out)
}

fn parse_stss(body: &[u8]) -> Result<Vec<u32>, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, 4)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(c.u32()?);
    }
    Ok(out)
}

fn parse_stsc(body: &[u8]) -> Result<Vec<(u32, u32, u32)>, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, 12)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let first = c.u32()?;
        let per_chunk = c.u32()?;
        let entry = c.u32()?;
        out.push((first, per_chunk, entry));
    }
    Ok(out)
}

fn parse_stsz(body: &[u8]) -> Result<Sizes, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let uniform = c.u32()?;
    let raw = c.u32()?;
    let count = sample_count(raw)?;
    let table = if uniform == 0 {
        c.count(raw, 4)?;
        let mut t = Vec::with_capacity(count);
        for _ in 0..count {
            t.push(c.u32()?);
        }
        t
    } else {
        Vec::new()
    };
    Ok(Sizes {
        uniform,
        count,
        table,
    })
}

fn parse_stz2(body: &[u8]) -> Result<Sizes, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    c.skip(3)?; // reserved
    let field = usize::from(c.u8()?);
    let raw = c.u32()?;
    let count = sample_count(raw)?;
    if !matches!(field, 4 | 8 | 16) {
        return Err(bad("unsupported stz2 field size"));
    }
    let bits = count
        .checked_mul(field)
        .ok_or_else(|| bad("stz2 table too large"))?;
    let packed = c.take(bits.div_ceil(8))?;
    let table: Vec<u32> = (0..count)
        .map(|i| match field {
            4 => {
                let b = packed.get(i / 2).copied().unwrap_or(0);
                u32::from(if i % 2 == 0 { b >> 4 } else { b & 0x0F })
            }
            8 => u32::from(packed.get(i).copied().unwrap_or(0)),
            _ => {
                let hi = packed.get(2 * i).copied().unwrap_or(0);
                let lo = packed.get(2 * i + 1).copied().unwrap_or(0);
                u32::from(u16::from_be_bytes([hi, lo]))
            }
        })
        .collect();
    Ok(Sizes {
        uniform: 0,
        count,
        table,
    })
}

fn parse_chunk_offsets(body: &[u8], co64: bool) -> Result<Vec<u64>, Mp4Error> {
    let mut c = Cur::new(body);
    c.fullbox()?;
    let raw = c.u32()?;
    let n = c.count(raw, if co64 { 8 } else { 4 })?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(if co64 { c.u64()? } else { u64::from(c.u32()?) });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fragments (moof)
// ---------------------------------------------------------------------------

/// `tfhd` fields of one fragment track.
struct TfHd {
    track_id: u32,
    base: Option<u64>,
    sdi: Option<u32>,
    duration: Option<u32>,
    size: Option<u32>,
    flags: Option<u32>,
    duration_empty: bool,
}

fn parse_tfhd(body: &[u8]) -> Result<TfHd, Mp4Error> {
    let mut c = Cur::new(body);
    let (_, f) = c.fullbox()?;
    let track_id = c.u32()?;
    let base = if f & 0x01 != 0 { Some(c.u64()?) } else { None };
    let sdi = if f & 0x02 != 0 { Some(c.u32()?) } else { None };
    let duration = if f & 0x08 != 0 { Some(c.u32()?) } else { None };
    let size = if f & 0x10 != 0 { Some(c.u32()?) } else { None };
    let flags = if f & 0x20 != 0 { Some(c.u32()?) } else { None };
    Ok(TfHd {
        track_id,
        base,
        sdi,
        duration,
        size,
        flags,
        duration_empty: f & 0x0001_0000 != 0,
    })
}

fn parse_tfdt(body: &[u8]) -> Result<i64, Mp4Error> {
    let mut c = Cur::new(body);
    let (v, _) = c.fullbox()?;
    let t = if v == 1 {
        c.u64()?
    } else {
        u64::from(c.u32()?)
    };
    i64::try_from(t).map_err(|_| bad("decode time out of range"))
}

/// Per-traf constants shared by its `trun` boxes.
struct RunCtx<'a> {
    hd: &'a TfHd,
    trex: Option<Trex>,
    /// Base for data offsets (tfhd base_data_offset, else the moof start).
    base: u64,
}

/// Per-traf running state.
struct RunState {
    /// File offset where the next sample without data_offset starts.
    cursor: u64,
    /// Decode time of the next sample.
    dts: i64,
}

fn parse_moof(body: &[u8], moof_pos: u64, p: &mut Pending) -> Result<(), Mp4Error> {
    for t in boxes(body)? {
        if &t.typ == b"traf" {
            parse_traf(t.body, moof_pos, p)?;
        }
    }
    Ok(())
}

fn parse_traf(body: &[u8], moof_pos: u64, p: &mut Pending) -> Result<(), Mp4Error> {
    let kids = boxes(body)?;
    let tfhd_box = kids
        .iter()
        .find(|k| &k.typ == b"tfhd")
        .ok_or_else(|| bad("traf without tfhd"))?;
    let hd = parse_tfhd(tfhd_box.body)?;
    let trex = p
        .trex
        .iter()
        .find(|(id, _)| *id == hd.track_id)
        .map(|(_, t)| *t);
    let tfdt = kids
        .iter()
        .find(|k| &k.typ == b"tfdt")
        .map(|k| parse_tfdt(k.body))
        .transpose()?;
    // Fragments of tracks not in the moov are ignored.
    let Some(track) = p.tracks.iter_mut().find(|t| t.id == hd.track_id) else {
        return Ok(());
    };
    let end = match track.samples.last() {
        Some(s) => s
            .dts
            .checked_add(i64::from(s.duration))
            .ok_or_else(|| bad("decode time overflows"))?,
        None => 0,
    };
    // Without tfdt the decode time continues from the track; never go backwards.
    let dts = match tfdt {
        Some(t) => t.max(end),
        None => end,
    };
    let base = hd.base.unwrap_or(moof_pos);
    let ctx = RunCtx {
        hd: &hd,
        trex,
        base,
    };
    let mut st = RunState { cursor: base, dts };
    for trun in kids.iter().filter(|k| &k.typ == b"trun") {
        parse_trun(trun.body, &ctx, &mut st, track)?;
    }
    Ok(())
}

fn parse_trun(
    body: &[u8],
    ctx: &RunCtx<'_>,
    st: &mut RunState,
    track: &mut Track,
) -> Result<(), Mp4Error> {
    let mut c = Cur::new(body);
    let (ver, f) = c.fullbox()?;
    let raw_count = c.u32()?;
    let data_offset = if f & 0x001 != 0 { Some(c.i32()?) } else { None };
    let first_flags = if f & 0x004 != 0 { Some(c.u32()?) } else { None };
    let has_dur = f & 0x100 != 0;
    let has_size = f & 0x200 != 0;
    let has_flags = f & 0x400 != 0;
    let has_cts = f & 0x800 != 0;
    let per_sample = [has_dur, has_size, has_flags, has_cts]
        .iter()
        .filter(|&&b| b)
        .count()
        * 4;
    let n = c.count(raw_count, per_sample)?;
    if track.samples.len().saturating_add(n) > MAX_SAMPLES {
        return Err(bad("track has too many samples"));
    }

    let trex = ctx.trex;
    let hd = ctx.hd;
    let mut pos = match data_offset {
        Some(off) => ctx
            .base
            .checked_add_signed(i64::from(off))
            .ok_or_else(|| bad("data offset out of range"))?,
        None => st.cursor,
    };
    let dur_default = if hd.duration_empty {
        0
    } else {
        hd.duration.or(trex.map(|t| t.duration)).unwrap_or(0)
    };
    let size_default = hd.size.or(trex.map(|t| t.size)).unwrap_or(0);
    let flags_default = hd.flags.or(trex.map(|t| t.flags)).unwrap_or(0);
    let entry = hd.sdi.or(trex.map(|t| t.sdi)).unwrap_or(1);

    track.samples.reserve(n.min(1 << 16));
    for i in 0..n {
        let duration = if has_dur { c.u32()? } else { dur_default };
        let size = if has_size { c.u32()? } else { size_default };
        let sflags = if has_flags { c.u32()? } else { flags_default };
        let sflags = match first_flags {
            Some(ff) if i == 0 => ff,
            _ => sflags,
        };
        let cts_offset = if has_cts {
            if ver == 0 {
                i32::try_from(c.u32()?).unwrap_or(i32::MAX)
            } else {
                c.i32()?
            }
        } else {
            0
        };
        track.samples.push(SampleRef {
            offset: pos,
            size,
            dts: st.dts,
            cts_offset,
            duration,
            keyframe: (sflags & 0x0001_0000) == 0,
            entry,
        });
        pos = pos
            .checked_add(u64::from(size))
            .ok_or_else(|| bad("sample offset overflows"))?;
        st.dts = st
            .dts
            .checked_add(i64::from(duration))
            .ok_or_else(|| bad("decode time overflows"))?;
    }
    st.cursor = pos;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aac::aac_lc_asc;
    use crate::avc::avc_config;
    use crate::fmp4::{
        write_fragment, write_init, AudioTrackSpec, InitSpec, Sample, TrackRun, VideoConfig,
        VideoTrackSpec,
    };
    use std::io::Cursor;

    /// Annex B SPS + PPS of a 1280x720 High profile stream.
    const PARAM_SETS: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01,
        0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60,
        0x00, 0x00, 0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0,
    ];

    fn video_samples() -> Vec<Vec<u8>> {
        vec![vec![1u8; 10], vec![2u8; 20], vec![3u8; 30]]
    }

    fn audio_samples() -> Vec<Vec<u8>> {
        vec![vec![0xA1u8; 5], vec![0xA2u8; 7]]
    }

    fn sample(data: Vec<u8>, duration: u32, keyframe: bool) -> Sample {
        Sample {
            data,
            duration,
            cts_offset: 0,
            keyframe,
        }
    }

    /// Init segment, fragment 1 (video run + first audio run), fragment 2 (second audio run).
    fn build() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let cfg = avc_config(PARAM_SETS).expect("sps and pps");
        let init = write_init(&InitSpec {
            video: VideoTrackSpec {
                config: VideoConfig::Avc(cfg),
                width: 1280,
                height: 720,
                timescale: 90_000,
            },
            audio: vec![AudioTrackSpec {
                sample_rate: 48_000,
                channels: 2,
                asc: aac_lc_asc(48_000, 2).expect("asc"),
                bitrate: 128_000,
                title: "Game".to_string(),
            }],
        });
        let v = video_samples();
        let a = audio_samples();
        let video_run = TrackRun {
            track_id: 1,
            base_decode_time: 0,
            samples: v
                .iter()
                .enumerate()
                .map(|(i, d)| sample(d.clone(), 3000, i == 0))
                .collect(),
        };
        let audio_run0 = TrackRun {
            track_id: 2,
            base_decode_time: 0,
            samples: vec![sample(a[0].clone(), 1024, true)],
        };
        let f1 = write_fragment(1, &[video_run, audio_run0]);
        let audio_run1 = TrackRun {
            track_id: 2,
            base_decode_time: 1024,
            samples: vec![sample(a[1].clone(), 1024, true)],
        };
        let f2 = write_fragment(2, &[audio_run1]);
        (init, f1, f2)
    }

    fn whole_file() -> Vec<u8> {
        let (init, f1, f2) = build();
        [init, f1, f2].concat()
    }

    #[test]
    fn round_trip_of_own_writer() {
        let file = whole_file();
        let m = read_movie_from(Cursor::new(file.clone())).expect("read");
        assert_eq!(m.tracks.len(), 2);

        let v = &m.tracks[0];
        assert_eq!(v.id, 1);
        assert_eq!(v.timescale, 90_000);
        assert_eq!(
            v.kind,
            TrackKind::Video {
                codec: "h264".to_string(),
                width: 1280,
                height: 720,
            }
        );
        assert!(v.title.is_none());
        assert!(v.is_video());
        assert_eq!(&v.sample_entries[0][4..8], b"avc1");
        assert_eq!(v.samples.len(), 3);
        let sizes: Vec<u32> = v.samples.iter().map(|s| s.size).collect();
        assert_eq!(sizes, vec![10, 20, 30]);
        let dts: Vec<i64> = v.samples.iter().map(|s| s.dts).collect();
        assert_eq!(dts, vec![0, 3000, 6000]);
        let keys: Vec<bool> = v.samples.iter().map(|s| s.keyframe).collect();
        assert_eq!(keys, vec![true, false, false]);
        assert!(v.samples.iter().all(|s| s.duration == 3000 && s.entry == 1));
        for (s, data) in v.samples.iter().zip(video_samples()) {
            let off = s.offset as usize;
            assert_eq!(&file[off..off + s.size as usize], &data[..]);
        }

        let a = &m.tracks[1];
        assert_eq!(a.id, 2);
        assert_eq!(a.timescale, 48_000);
        assert_eq!(
            a.kind,
            TrackKind::Audio {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48_000,
            }
        );
        assert_eq!(a.title.as_deref(), Some("Game"));
        assert!(a.is_audio());
        assert_eq!(a.samples.len(), 2);
        let adts: Vec<i64> = a.samples.iter().map(|s| s.dts).collect();
        assert_eq!(adts, vec![0, 1024]);
        assert!(a.samples.iter().all(|s| s.keyframe));
        for (s, data) in a.samples.iter().zip(audio_samples()) {
            let off = s.offset as usize;
            assert_eq!(&file[off..off + s.size as usize], &data[..]);
        }

        assert!((v.duration_secs() - 0.1).abs() < 1e-9);
        assert!((m.duration_secs - 0.1).abs() < 1e-9);
    }

    #[test]
    fn truncated_second_fragment_keeps_first() {
        let (init, f1, f2) = build();
        let file = [init.clone(), f1.clone(), f2.clone()].concat();

        // Cut in the middle of fragment 2: its moof is incomplete.
        let cut = init.len() + f1.len() + f2.len() / 2;
        let m = read_movie_from(Cursor::new(file[..cut].to_vec())).expect("read");
        assert_eq!(m.tracks[0].samples.len(), 3);
        assert_eq!(m.tracks[1].samples.len(), 1);
        assert_eq!(m.tracks[1].samples[0].dts, 0);

        // Cut inside fragment 2's mdat: its moof is complete but its sample bytes are not.
        let cut = init.len() + f1.len() + f2.len() - 3;
        let m = read_movie_from(Cursor::new(file[..cut].to_vec())).expect("read");
        assert_eq!(m.tracks[0].samples.len(), 3);
        assert_eq!(m.tracks[1].samples.len(), 1);
    }

    #[test]
    fn prefixes_and_garbage_never_panic() {
        let file = whole_file();
        for n in 0..=file.len() {
            let _ = read_movie_from(Cursor::new(file[..n].to_vec()));
        }
        for i in 0..file.len() {
            let mut corrupt = file.clone();
            corrupt[i] ^= 0xFF;
            let _ = read_movie_from(Cursor::new(corrupt));
        }
        let mut seed: u32 = 0x1234_5678;
        for len in 0..300usize {
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                buf.push((seed >> 16) as u8);
            }
            let _ = read_movie_from(Cursor::new(buf));
        }
    }
}
