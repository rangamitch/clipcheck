//! Fragmented MP4 box writer: init segment and media fragments (worker M3).

use crate::avc::{avcc_payload, AvcConfig};
use crate::hevc::{hvcc_payload, HevcConfig};

/// Identity transformation matrix used by `mvhd` and `tkhd`.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// Packed ISO 639-2 language code "und".
const LANG_UND: u16 =
    (((b'u' - 0x60) as u16) << 10) | (((b'n' - 0x60) as u16) << 5) | ((b'd' - 0x60) as u16);

/// A plain box: u32 size (header included), four-character type, payload.
pub(crate) fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let total = payload.len() + 8;
    debug_assert!(u32::try_from(total).is_ok(), "box larger than 4 GiB");
    let size = u32::try_from(total).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(payload);
    out
}

/// A full box: `bx` with a u8 version and u24 flags in front of the payload.
pub(crate) fn full(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 4);
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    bx(typ, &body)
}

/// Big-endian bytes of a list of u32 values.
fn u32s(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// MPEG-4 descriptor: tag, then the length as four bytes (0x80 continuation bits).
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX) & 0x0FFF_FFFF;
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(tag);
    out.push(0x80 | ((len >> 21) & 0x7F) as u8);
    out.push(0x80 | ((len >> 14) & 0x7F) as u8);
    out.push(0x80 | ((len >> 7) & 0x7F) as u8);
    out.push((len & 0x7F) as u8);
    out.extend_from_slice(body);
    out
}

/// `mvhd` (version 0) with the given next track id.
fn mvhd(next_track_id: u32) -> Vec<u8> {
    let mut p = u32s(&[0, 0, 1000, 0, 0x0001_0000]);
    p.extend_from_slice(&0x0100u16.to_be_bytes());
    p.extend_from_slice(&[0u8; 10]);
    p.extend_from_slice(&u32s(&MATRIX));
    p.extend_from_slice(&u32s(&[0; 6]));
    p.extend_from_slice(&next_track_id.to_be_bytes());
    full(b"mvhd", 0, 0, &p)
}

/// `tkhd` (version 0, flags 3). `dims` is `Some((width, height))` for video.
fn tkhd(track_id: u32, dims: Option<(u32, u32)>) -> Vec<u8> {
    let is_video = dims.is_some();
    let (width, height) = dims.unwrap_or((0, 0));
    let mut p = u32s(&[0, 0, track_id, 0, 0, 0, 0]);
    p.extend_from_slice(&0u16.to_be_bytes()); // layer
    let alternate: u16 = if is_video { 0 } else { 1 };
    let volume: u16 = if is_video { 0 } else { 0x0100 };
    p.extend_from_slice(&alternate.to_be_bytes());
    p.extend_from_slice(&volume.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&u32s(&MATRIX));
    p.extend_from_slice(&u32s(&[width << 16, height << 16]));
    full(b"tkhd", 0, 3, &p)
}

/// `mdhd` (version 0).
fn mdhd(timescale: u32) -> Vec<u8> {
    let mut p = u32s(&[0, 0, timescale, 0]);
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
    let dref_p = [u32s(&[1]), url].concat();
    bx(b"dinf", &full(b"dref", 0, 0, &dref_p))
}

/// `stbl` with one sample entry and empty sample tables.
fn stbl(entry: Vec<u8>) -> Vec<u8> {
    let stsd = full(b"stsd", 0, 0, &[u32s(&[1]), entry].concat());
    let stts = full(b"stts", 0, 0, &u32s(&[0]));
    let stsc = full(b"stsc", 0, 0, &u32s(&[0]));
    let stsz = full(b"stsz", 0, 0, &u32s(&[0, 0]));
    let stco = full(b"stco", 0, 0, &u32s(&[0]));
    bx(b"stbl", &[stsd, stts, stsc, stsz, stco].concat())
}

/// `minf` from a media header box, the `dinf` and an `stbl`.
fn minf(media_header: Vec<u8>, entry: Vec<u8>) -> Vec<u8> {
    bx(b"minf", &[media_header, dinf(), stbl(entry)].concat())
}

/// `mdia` from a media header, a handler and a `minf`.
fn mdia(timescale: u32, handler: &[u8; 4], name: &str, minf_box: Vec<u8>) -> Vec<u8> {
    bx(
        b"mdia",
        &[mdhd(timescale), hdlr(handler, name), minf_box].concat(),
    )
}

/// `VisualSampleEntry` box (`avc1` or `hvc1`) wrapping its codec child box.
fn visual_entry(fourcc: &[u8; 4], width: u32, height: u32, child: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 6]; // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(&[0u8; 16]); // pre_defined, reserved, 3x pre_defined
    p.extend_from_slice(&u16::try_from(width).unwrap_or(u16::MAX).to_be_bytes());
    p.extend_from_slice(&u16::try_from(height).unwrap_or(u16::MAX).to_be_bytes());
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution 72 dpi
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution 72 dpi
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    p.extend_from_slice(&[0u8; 32]); // compressorname
    p.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    p.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined = -1
    p.extend_from_slice(child);
    bx(fourcc, &p)
}

/// `AudioSampleEntry` (`mp4a`) with its `esds` child.
fn audio_entry(a: &AudioTrackSpec) -> Vec<u8> {
    let dsi = descriptor(0x05, &a.asc);
    let mut dcd_body = vec![0x40, 0x15, 0, 0, 0]; // objectTypeIndication, streamType, bufferSizeDB
    dcd_body.extend_from_slice(&u32s(&[a.bitrate, a.bitrate]));
    dcd_body.extend_from_slice(&dsi);
    let dcd = descriptor(0x04, &dcd_body);
    let sl = descriptor(0x06, &[0x02]);
    let mut es_body = vec![0, 0, 0]; // ES_ID, flags
    es_body.extend_from_slice(&dcd);
    es_body.extend_from_slice(&sl);
    let esds = full(b"esds", 0, 0, &descriptor(0x03, &es_body));

    let mut p = vec![0u8; 6]; // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(&u32s(&[0, 0])); // reserved
    p.extend_from_slice(&a.channels.to_be_bytes());
    p.extend_from_slice(&16u16.to_be_bytes()); // samplesize
    p.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    let samplerate = if a.sample_rate <= 65535 {
        a.sample_rate << 16
    } else {
        0
    };
    p.extend_from_slice(&samplerate.to_be_bytes());
    p.extend_from_slice(&esds);
    bx(b"mp4a", &p)
}

/// `trak` for the video track (track id 1).
fn trak_video(v: &VideoTrackSpec) -> Vec<u8> {
    let entry = match &v.config {
        VideoConfig::Avc(c) => {
            let avcc = bx(b"avcC", &avcc_payload(c));
            visual_entry(b"avc1", v.width, v.height, &avcc)
        }
        VideoConfig::Hevc(c) => {
            let hvcc = bx(b"hvcC", &hvcc_payload(c));
            visual_entry(b"hvc1", v.width, v.height, &hvcc)
        }
    };
    let vmhd = full(b"vmhd", 0, 1, &[0u8; 8]);
    let mdia_box = mdia(v.timescale, b"vide", "VideoHandler", minf(vmhd, entry));
    bx(
        b"trak",
        &[tkhd(1, Some((v.width, v.height))), mdia_box].concat(),
    )
}

/// `trak` for one audio track, with `udta/name` when it has a title.
fn trak_audio(a: &AudioTrackSpec, track_id: u32) -> Vec<u8> {
    let smhd = full(b"smhd", 0, 0, &[0u8; 4]);
    let mdia_box = mdia(
        a.sample_rate,
        b"soun",
        "SoundHandler",
        minf(smhd, audio_entry(a)),
    );
    let mut body = [tkhd(track_id, None), mdia_box].concat();
    if !a.title.is_empty() {
        body.extend_from_slice(&bx(b"udta", &bx(b"name", a.title.as_bytes())));
    }
    bx(b"trak", &body)
}

/// `trex` with default sample description index 1 and all other defaults 0.
fn trex(track_id: u32) -> Vec<u8> {
    full(b"trex", 0, 0, &u32s(&[track_id, 1, 0, 0, 0]))
}

/// Total sample payload bytes of a run.
fn run_bytes(run: &TrackRun) -> usize {
    run.samples.iter().map(|s| s.data.len()).sum()
}

/// `traf` for one run. `data_offset` is the offset of the run's first byte from the start of `moof`.
fn traf(run: &TrackRun, data_offset: i32) -> Vec<u8> {
    let tfhd = full(b"tfhd", 0, 0x0002_0000, &u32s(&[run.track_id]));
    let tfdt = full(b"tfdt", 1, 0, &run.base_decode_time.to_be_bytes());
    let mut p = u32s(&[u32::try_from(run.samples.len()).unwrap_or(u32::MAX)]);
    p.extend_from_slice(&data_offset.to_be_bytes());
    for s in &run.samples {
        let size = u32::try_from(s.data.len()).unwrap_or(u32::MAX);
        let flags: u32 = if s.keyframe { 0x0200_0000 } else { 0x0101_0000 };
        p.extend_from_slice(&u32s(&[s.duration, size, flags]));
        p.extend_from_slice(&s.cts_offset.to_be_bytes());
    }
    let trun = full(b"trun", 1, 0x000F01, &p);
    bx(b"traf", &[tfhd, tfdt, trun].concat())
}

/// `moof` for the runs; `offsets[i]` is the data offset of run `i`.
fn build_moof(sequence_number: u32, runs: &[&TrackRun], offsets: &[i32]) -> Vec<u8> {
    let mut parts = vec![full(b"mfhd", 0, 0, &u32s(&[sequence_number]))];
    for (run, off) in runs.iter().zip(offsets) {
        parts.push(traf(run, *off));
    }
    bx(b"moof", &parts.concat())
}

/// `ftyp` + `moov` for `spec`.
pub fn write_init(spec: &InitSpec) -> Vec<u8> {
    let brand: &[u8; 4] = match &spec.video.config {
        VideoConfig::Avc(_) => b"avc1",
        VideoConfig::Hevc(_) => b"hvc1",
    };
    let mut ftyp_p = Vec::new();
    ftyp_p.extend_from_slice(b"isom");
    ftyp_p.extend_from_slice(&0x200u32.to_be_bytes());
    for brand_id in [b"isom", b"iso2", b"iso6", brand, b"mp41"] {
        ftyp_p.extend_from_slice(brand_id);
    }
    let ftyp = bx(b"ftyp", &ftyp_p);

    let audio_count = u32::try_from(spec.audio.len()).unwrap_or(u32::MAX - 2);
    let mut moov_p = mvhd(audio_count + 2);
    moov_p.extend_from_slice(&trak_video(&spec.video));
    // Audio traks are written in input order (Mix = 2 is first), matching track ids. Discord
    // embeds play the first audio trak only.
    for (i, a) in spec.audio.iter().enumerate() {
        let track_id = u32::try_from(i).unwrap_or(u32::MAX - 2).saturating_add(2);
        moov_p.extend_from_slice(&trak_audio(a, track_id));
    }
    let mut mvex_p = trex(1);
    for i in 0..spec.audio.len() {
        let track_id = u32::try_from(i).unwrap_or(u32::MAX - 2).saturating_add(2);
        mvex_p.extend_from_slice(&trex(track_id));
    }
    moov_p.extend_from_slice(&bx(b"mvex", &mvex_p));

    [ftyp, bx(b"moov", &moov_p)].concat()
}

/// `moof` + `mdat` for `runs`. Empty when there are no samples.
pub fn write_fragment(sequence_number: u32, runs: &[TrackRun]) -> Vec<u8> {
    let runs: Vec<&TrackRun> = runs.iter().filter(|r| !r.samples.is_empty()).collect();
    if runs.is_empty() {
        return Vec::new();
    }
    // The moof size does not depend on the offset values, so measure it first.
    let placeholder = vec![0i32; runs.len()];
    let moof_len = build_moof(sequence_number, &runs, &placeholder).len();

    // Run k's data starts after the moof, the 8-byte mdat header and all earlier runs.
    let mut offsets = Vec::with_capacity(runs.len());
    let mut pos = moof_len + 8;
    for run in &runs {
        offsets.push(i32::try_from(pos).unwrap_or(i32::MAX));
        pos += run_bytes(run);
    }
    let moof = build_moof(sequence_number, &runs, &offsets);
    debug_assert_eq!(moof.len(), moof_len);

    let mut payload = Vec::with_capacity(pos - moof_len - 8);
    for run in &runs {
        for s in &run.samples {
            payload.extend_from_slice(&s.data);
        }
    }
    [moof, bx(b"mdat", &payload)].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(data: &[u8], keyframe: bool) -> Sample {
        Sample {
            data: data.to_vec(),
            duration: 3000,
            cts_offset: 0,
            keyframe,
        }
    }

    fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
        hay.windows(needle.len())
            .enumerate()
            .filter(|(_, w)| *w == needle)
            .map(|(i, _)| i)
            .collect()
    }

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[test]
    fn empty_runs_give_empty_fragment() {
        let empty = TrackRun {
            track_id: 1,
            base_decode_time: 0,
            samples: Vec::new(),
        };
        assert!(write_fragment(1, &[empty]).is_empty());
        assert!(write_fragment(1, &[]).is_empty());
    }

    #[test]
    fn fragment_data_offsets_point_at_sample_bytes() {
        let video = TrackRun {
            track_id: 1,
            base_decode_time: 9000,
            samples: vec![sample(&[1, 2, 3], true), sample(&[4, 5], false)],
        };
        let audio = TrackRun {
            track_id: 2,
            base_decode_time: 48000,
            samples: vec![sample(&[9, 9, 9, 9], true)],
        };
        let out = write_fragment(7, &[video, audio]);

        let moof_size = be32(&out, 0) as usize;
        assert_eq!(&out[4..8], b"moof");
        assert_eq!(&out[moof_size + 4..moof_size + 8], b"mdat");
        assert_eq!(be32(&out, moof_size) as usize, out.len() - moof_size);
        assert_eq!(&out[moof_size + 8..], &[1, 2, 3, 4, 5, 9, 9, 9, 9]);

        // trun payload: version/flags, sample_count, data_offset.
        let truns = find_all(&out[..moof_size], b"trun");
        assert_eq!(truns.len(), 2);
        let first = truns[0];
        assert_eq!(be32(&out, first + 8), 2);
        let off0 = be32(&out, first + 12) as i32;
        assert_eq!(off0 as usize, moof_size + 8);
        assert_eq!(&out[off0 as usize..off0 as usize + 5], &[1, 2, 3, 4, 5]);

        let second = truns[1];
        let off1 = be32(&out, second + 12) as i32;
        assert_eq!(off1 as usize, moof_size + 8 + 5);
        assert_eq!(&out[off1 as usize..off1 as usize + 4], &[9, 9, 9, 9]);
    }

    #[test]
    fn descriptor_length_is_four_bytes() {
        let d = descriptor(0x05, &[0xAA, 0xBB]);
        assert_eq!(d, vec![0x05, 0x80, 0x80, 0x80, 0x02, 0xAA, 0xBB]);
    }
}

/// Codec configuration of the video track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoConfig {
    /// H.264 (`avc1` + `avcC`).
    Avc(AvcConfig),
    /// H.265 (`hvc1` + `hvcC`).
    Hevc(HevcConfig),
}

/// The video track of an init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrackSpec {
    /// Codec configuration.
    pub config: VideoConfig,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Media timescale (ticks per second), e.g. 90 000.
    pub timescale: u32,
}

/// One AAC audio track of an init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackSpec {
    /// Sample rate in Hz; also the track timescale.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// AudioSpecificConfig bytes.
    pub asc: Vec<u8>,
    /// Average bitrate in bit/s (for `esds`).
    pub bitrate: u32,
    /// Track title stored in `udta/name`; empty for none.
    pub title: String,
}

/// Tracks of an init segment. Track ids: video = 1, audio[i] = i + 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSpec {
    /// The video track.
    pub video: VideoTrackSpec,
    /// Audio tracks in order.
    pub audio: Vec<AudioTrackSpec>,
}

/// One sample of a track run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Sample bytes (length-prefixed NALs for video, a raw AAC frame for audio).
    pub data: Vec<u8>,
    /// Duration in the track timescale.
    pub duration: u32,
    /// Composition time offset (pts - dts) in the track timescale.
    pub cts_offset: i32,
    /// Sync sample.
    pub keyframe: bool,
}

/// Samples of one track inside one fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRun {
    /// Track id (1 = video, 2.. = audio).
    pub track_id: u32,
    /// Decode time of the first sample, in the track timescale.
    pub base_decode_time: u64,
    /// Samples in decode order.
    pub samples: Vec<Sample>,
}
