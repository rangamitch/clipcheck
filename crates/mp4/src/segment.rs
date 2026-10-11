//! Segment muxer: `seg-NNNNNN.mp4` files plus `segments.csv`, written in process (worker M4).

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::annexb::to_length_prefixed;
use crate::avc::avc_config;
use crate::fmp4::{
    write_fragment, write_init, AudioTrackSpec, InitSpec, Sample, TrackRun, VideoConfig,
    VideoTrackSpec,
};
use crate::hevc::hevc_config;
use crate::{Mp4Error, VideoCodec};

/// Video track timescale (90 kHz).
const VIDEO_TIMESCALE: u32 = 90_000;
/// 100 ns units per second.
const UNITS_PER_SEC: f64 = 1e7;

/// Muxer configuration.
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    /// Directory receiving the segments and `segments.csv`. Created if missing.
    pub out_dir: PathBuf,
    /// Target segment length in seconds (> 0).
    pub segment_secs: f64,
    /// Nominal frame rate (> 0), used for the last video sample's duration.
    pub fps: u32,
    /// Video codec of the packets.
    pub codec: VideoCodec,
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Audio tracks, AudioSpecificConfig filled in.
    pub audio: Vec<AudioTrackSpec>,
}

/// One encoded video access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket {
    /// Annex B access unit.
    pub data: Vec<u8>,
    /// Presentation time, 100 ns.
    pub pts: i64,
    /// Decode time, 100 ns.
    pub dts: i64,
    /// IDR/IRAP access unit.
    pub keyframe: bool,
}

/// One encoded AAC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    /// Raw AAC frame (no ADTS header).
    pub data: Vec<u8>,
    /// Presentation time, 100 ns.
    pub pts: i64,
    /// PCM samples per channel in the frame (1024 for AAC-LC).
    pub samples: u32,
}

/// A video sample whose duration is not yet known (the next packet has not arrived).
#[derive(Debug)]
struct OpenSample {
    /// Length-prefixed sample data.
    data: Vec<u8>,
    /// Decode time in 90 kHz units.
    dts: i64,
    /// Composition offset in 90 kHz units.
    cts_offset: i32,
    keyframe: bool,
}

/// The segment file currently being written.
#[derive(Debug)]
struct OpenSegment {
    writer: BufWriter<File>,
    /// Presentation time of the segment start, 100 ns.
    start_pts: i64,
    /// Decode time of the segment's first keyframe, 90 kHz units.
    video_base: i64,
    /// Per-audio-track decode time counters, in each track's timescale.
    audio_time: Vec<u64>,
}

/// Writes keyframe-aligned fragmented MP4 segments.
#[derive(Debug)]
pub struct SegmentMuxer {
    cfg: SegmentConfig,
    /// Video packets dropped before the first keyframe.
    dropped: u64,
    /// Init segment description, set at the first keyframe.
    init: Option<InitSpec>,
    /// Session origin: pts of the first keyframe, 100 ns.
    t0: Option<i64>,
    current: Option<OpenSegment>,
    /// Latest video packet, waiting for its duration.
    open_sample: Option<OpenSample>,
    /// Finalized video samples of the pending fragment.
    video_samples: Vec<Sample>,
    /// Decode time (90 kHz) of the first sample in `video_samples`.
    fragment_first_dts: i64,
    /// Audio packets waiting for a fragment, per track.
    audio_queues: Vec<VecDeque<AudioPacket>>,
    /// Index of the next segment file.
    next_index: u32,
    /// Sequence number of the next fragment.
    next_seq: u32,
    /// Paths of all segments opened so far.
    paths: Vec<PathBuf>,
    /// Presentation time of the latest video packet, 100 ns.
    last_pts: i64,
}

/// Converts 100 ns units to 90 kHz ticks, rounding to nearest.
fn to_90k(t: i64) -> i64 {
    let n = i128::from(t) * 9;
    let q = if n >= 0 {
        (n + 500) / 1000
    } else {
        -((-n + 500) / 1000)
    };
    i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX })
}

/// Name of segment `index`.
fn segment_name(index: u32) -> String {
    format!("seg-{index:06}.mp4")
}

impl SegmentMuxer {
    /// Validates `cfg` and creates the output directory.
    pub fn new(cfg: SegmentConfig) -> Result<Self, Mp4Error> {
        if !cfg.segment_secs.is_finite() || cfg.segment_secs <= 0.0 {
            return Err(Mp4Error::InvalidConfig(format!(
                "segment_secs must be a positive finite number, got {}",
                cfg.segment_secs
            )));
        }
        if cfg.fps == 0 {
            return Err(Mp4Error::InvalidConfig("fps must be > 0".into()));
        }
        if cfg.width == 0 || cfg.height == 0 {
            return Err(Mp4Error::InvalidConfig(format!(
                "invalid frame size {}x{}",
                cfg.width, cfg.height
            )));
        }
        std::fs::create_dir_all(&cfg.out_dir)?;
        let audio_queues = (0..cfg.audio.len()).map(|_| VecDeque::new()).collect();
        Ok(Self {
            cfg,
            dropped: 0,
            init: None,
            t0: None,
            current: None,
            open_sample: None,
            video_samples: Vec::new(),
            fragment_first_dts: 0,
            audio_queues,
            next_index: 0,
            next_seq: 1,
            paths: Vec::new(),
            last_pts: 0,
        })
    }

    /// Adds one video access unit.
    pub fn push_video(&mut self, pkt: VideoPacket) -> Result<(), Mp4Error> {
        let dts90 = to_90k(pkt.dts);
        let first = self.t0.is_none();
        if first {
            if !pkt.keyframe {
                self.dropped = self.dropped.saturating_add(1);
                return Ok(());
            }
            // Derive the codec configuration before changing any state.
            let config = match self.cfg.codec {
                VideoCodec::H264 => avc_config(&pkt.data).map(VideoConfig::Avc),
                VideoCodec::Hevc => hevc_config(&pkt.data).map(VideoConfig::Hevc),
            }
            .ok_or(Mp4Error::MissingParameterSets)?;
            self.init = Some(InitSpec {
                video: VideoTrackSpec {
                    config,
                    width: self.cfg.width,
                    height: self.cfg.height,
                    timescale: VIDEO_TIMESCALE,
                },
                audio: self.cfg.audio.clone(),
            });
            self.t0 = Some(pkt.pts);
            self.open_segment(pkt.pts, dts90)?;
        }
        let t0 = self.t0.unwrap_or(pkt.pts);

        // Finalize the previous sample now that the next packet's dts is known.
        if let Some(prev) = self.open_sample.take() {
            let duration = dts90.saturating_sub(prev.dts).max(1);
            self.finalize(prev, duration);
        }
        self.last_pts = pkt.pts;

        if pkt.keyframe && !first {
            // Keyframe: flush the pending fragment up to this keyframe, then maybe roll over.
            self.flush_fragment(pkt.pts)?;
            let threshold = self.cfg.segment_secs * UNITS_PER_SEC
                - (UNITS_PER_SEC / f64::from(self.cfg.fps)) / 2.0;
            let seg_start = self.current.as_ref().map_or(t0, |s| s.start_pts);
            let elapsed = pkt.pts.saturating_sub(seg_start) as f64;
            if elapsed >= threshold {
                self.close_segment(pkt.pts)?;
                self.open_segment(pkt.pts, dts90)?;
            }
        }

        let cts = to_90k(pkt.pts).saturating_sub(dts90);
        self.open_sample = Some(OpenSample {
            data: to_length_prefixed(&pkt.data, self.cfg.codec),
            dts: dts90,
            cts_offset: i32::try_from(cts).unwrap_or(if cts < 0 { i32::MIN } else { i32::MAX }),
            keyframe: pkt.keyframe,
        });
        Ok(())
    }

    /// Adds one AAC frame for audio track `track`.
    pub fn push_audio(&mut self, track: usize, pkt: AudioPacket) -> Result<(), Mp4Error> {
        if track >= self.cfg.audio.len() {
            return Err(Mp4Error::BadTrack(track));
        }
        match self.t0 {
            Some(t0) if pkt.pts >= t0 => {
                self.audio_queues[track].push_back(pkt);
            }
            _ => {}
        }
        Ok(())
    }

    /// Flushes and closes the open segment; returns every segment path in order.
    pub fn finish(mut self) -> Result<Vec<PathBuf>, Mp4Error> {
        if self.t0.is_none() {
            return Ok(Vec::new());
        }
        let frame = UNITS_PER_SEC / f64::from(self.cfg.fps);
        if let Some(prev) = self.open_sample.take() {
            let last_duration = (u64::from(VIDEO_TIMESCALE) + u64::from(self.cfg.fps) / 2)
                / u64::from(self.cfg.fps);
            let duration = i64::try_from(last_duration).unwrap_or(i64::MAX).max(1);
            self.finalize(prev, duration);
        }
        self.flush_fragment(i64::MAX)?;
        let end = self.last_pts.saturating_add(frame.round() as i64);
        self.close_segment(end)?;
        Ok(std::mem::take(&mut self.paths))
    }

    /// Video packets dropped because they came before the first keyframe.
    pub fn dropped_before_keyframe(&self) -> u64 {
        self.dropped
    }

    /// Turns an open sample into a pending fragment sample with the given duration.
    fn finalize(&mut self, prev: OpenSample, duration: i64) {
        if self.video_samples.is_empty() {
            self.fragment_first_dts = prev.dts;
        }
        self.video_samples.push(Sample {
            data: prev.data,
            duration: u32::try_from(duration).unwrap_or(u32::MAX),
            cts_offset: prev.cts_offset,
            keyframe: prev.keyframe,
        });
    }

    /// Creates the next segment file and writes its init segment.
    fn open_segment(&mut self, start_pts: i64, video_base: i64) -> Result<(), Mp4Error> {
        let index = self.next_index;
        let path = self.cfg.out_dir.join(segment_name(index));
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        if let Some(init) = self.init.as_ref() {
            writer.write_all(&write_init(init))?;
        }
        self.next_index = index.saturating_add(1);
        self.paths.push(path);
        self.current = Some(OpenSegment {
            writer,
            start_pts,
            video_base,
            audio_time: vec![0; self.cfg.audio.len()],
        });
        Ok(())
    }

    /// Writes the pending video samples and the queued audio before `end_pts` as one fragment.
    fn flush_fragment(&mut self, end_pts: i64) -> Result<(), Mp4Error> {
        let Some(seg) = self.current.as_mut() else {
            return Ok(());
        };
        let mut runs = Vec::new();
        if !self.video_samples.is_empty() {
            let base = self
                .fragment_first_dts
                .saturating_sub(seg.video_base)
                .max(0);
            runs.push(TrackRun {
                track_id: 1,
                base_decode_time: base as u64,
                samples: std::mem::take(&mut self.video_samples),
            });
        }
        for (i, queue) in self.audio_queues.iter_mut().enumerate() {
            let mut samples = Vec::new();
            while queue.front().is_some_and(|p| p.pts < end_pts) {
                if let Some(pkt) = queue.pop_front() {
                    samples.push(Sample {
                        data: pkt.data,
                        duration: pkt.samples,
                        cts_offset: 0,
                        keyframe: true,
                    });
                }
            }
            if samples.is_empty() {
                continue;
            }
            let counter = &mut seg.audio_time[i];
            let base = *counter;
            let added: u64 = samples.iter().map(|s| u64::from(s.duration)).sum();
            *counter = counter.saturating_add(added);
            runs.push(TrackRun {
                track_id: u32::try_from(i + 2).unwrap_or(u32::MAX),
                base_decode_time: base,
                samples,
            });
        }
        if runs.is_empty() {
            return Ok(());
        }
        let seq = self.next_seq;
        self.next_seq = seq.saturating_add(1);
        seg.writer.write_all(&write_fragment(seq, &runs))?;
        seg.writer.flush()?;
        Ok(())
    }

    /// Flushes and closes the current segment, then appends its row to `segments.csv`.
    fn close_segment(&mut self, end_pts: i64) -> Result<(), Mp4Error> {
        let Some(t0) = self.t0 else {
            return Ok(());
        };
        self.flush_fragment(end_pts)?;
        let Some(mut seg) = self.current.take() else {
            return Ok(());
        };
        seg.writer.flush()?;
        drop(seg.writer);
        let index = self.next_index.saturating_sub(1);
        let start = (seg.start_pts - t0) as f64 / UNITS_PER_SEC;
        let end = (end_pts - t0) as f64 / UNITS_PER_SEC;
        let line = format!("{},{:.6},{:.6}\n", segment_name(index), start, end);
        let mut csv = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.cfg.out_dir.join("segments.csv"))?;
        csv.write_all(line.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 30] = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01,
        0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60,
    ];
    const PPS: [u8; 10] = [0x00, 0x00, 0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
    const IDR: [u8; 10] = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x33, 0xFF];
    const NON_IDR: [u8; 8] = [0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x02, 0x03];

    fn keyframe_au() -> Vec<u8> {
        let mut v = SPS.to_vec();
        v.extend_from_slice(&PPS);
        v.extend_from_slice(&IDR);
        v
    }

    fn config(dir: &std::path::Path, audio: usize) -> SegmentConfig {
        SegmentConfig {
            out_dir: dir.to_path_buf(),
            segment_secs: 1.0,
            fps: 30,
            codec: VideoCodec::H264,
            width: 320,
            height: 240,
            audio: (0..audio)
                .map(|_| AudioTrackSpec {
                    sample_rate: 48_000,
                    channels: 2,
                    asc: vec![0x11, 0x90],
                    bitrate: 128_000,
                    title: "a".into(),
                })
                .collect(),
        }
    }

    fn video(i: i64, keyframe: bool) -> VideoPacket {
        let pts = i * 10_000_000 / 30;
        VideoPacket {
            data: if keyframe {
                keyframe_au()
            } else {
                NON_IDR.to_vec()
            },
            pts,
            dts: pts,
            keyframe,
        }
    }

    #[test]
    fn three_seconds_make_three_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SegmentMuxer::new(config(dir.path(), 0)).unwrap();
        for i in 0..90 {
            m.push_video(video(i, i % 30 == 0)).unwrap();
        }
        let paths = m.finish().unwrap();
        assert_eq!(paths.len(), 3);
        for p in &paths {
            let bytes = std::fs::read(p).unwrap();
            assert_eq!(&bytes[4..8], b"ftyp");
        }
        let csv = std::fs::read_to_string(dir.path().join("segments.csv")).unwrap();
        let rows: Vec<&str> = csv.lines().collect();
        assert_eq!(rows.len(), 3);
        for (k, row) in rows.iter().enumerate() {
            let f: Vec<&str> = row.split(',').collect();
            assert_eq!(f[0], format!("seg-{k:06}.mp4"));
            let start: f64 = f[1].parse().unwrap();
            let end: f64 = f[2].parse().unwrap();
            assert!((end - start - 1.0).abs() < 0.05, "row {row}");
        }
    }

    #[test]
    fn packets_before_keyframe_are_counted_and_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SegmentMuxer::new(config(dir.path(), 0)).unwrap();
        m.push_video(video(0, false)).unwrap();
        m.push_video(video(1, false)).unwrap();
        assert_eq!(m.dropped_before_keyframe(), 2);
        let paths = m.finish().unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn no_keyframe_yields_no_segments() {
        let dir = tempfile::tempdir().unwrap();
        let m = SegmentMuxer::new(config(dir.path(), 0)).unwrap();
        assert!(m.finish().unwrap().is_empty());
        assert!(!dir.path().join("segments.csv").exists());
    }

    #[test]
    fn first_keyframe_without_parameter_sets_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SegmentMuxer::new(config(dir.path(), 0)).unwrap();
        let bad = VideoPacket {
            data: IDR.to_vec(),
            pts: 0,
            dts: 0,
            keyframe: true,
        };
        assert!(matches!(
            m.push_video(bad),
            Err(Mp4Error::MissingParameterSets)
        ));
    }

    #[test]
    fn audio_track_out_of_range_is_bad_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SegmentMuxer::new(config(dir.path(), 0)).unwrap();
        let pkt = AudioPacket {
            data: vec![0x21, 0x00],
            pts: 0,
            samples: 1024,
        };
        assert!(matches!(m.push_audio(0, pkt), Err(Mp4Error::BadTrack(0))));
    }

    #[test]
    fn audio_is_muxed_into_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SegmentMuxer::new(config(dir.path(), 1)).unwrap();
        for i in 0..90 {
            m.push_video(video(i, i % 30 == 0)).unwrap();
        }
        for n in 0..300i64 {
            let pkt = AudioPacket {
                data: vec![0x21, 0x00, 0x49],
                pts: n * 1024 * 10_000_000 / 48_000,
                samples: 1024,
            };
            m.push_audio(0, pkt).unwrap();
        }
        assert_eq!(m.finish().unwrap().len(), 3);
    }

    #[test]
    fn rejects_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path(), 0);
        c.fps = 0;
        assert!(matches!(
            SegmentMuxer::new(c),
            Err(Mp4Error::InvalidConfig(_))
        ));
        let mut c = config(dir.path(), 0);
        c.segment_secs = f64::NAN;
        assert!(matches!(
            SegmentMuxer::new(c),
            Err(Mp4Error::InvalidConfig(_))
        ));
    }
}
