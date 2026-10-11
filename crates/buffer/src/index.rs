//! Pure in-memory ring index for replay-buffer segments.
//!
//! No file I/O and no clocks: every time value is an argument, in 100 ns
//! ticks. Eviction returns the removed metadata so the caller can delete the
//! backing files.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;

/// Ticks (100 ns units) per second. Single definition lives in `clipcheck_core::media`.
pub use clipcheck_core::media::TICKS_PER_SEC;

/// Metadata for one buffer segment. All times are 100 ns ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    /// Monotonic sequence number within a run.
    pub seq: u64,
    /// First packet timestamp of the segment.
    pub start_100ns: i64,
    /// End timestamp. Provisional while the segment is open.
    pub end_100ns: i64,
    /// Timestamp of the segment's first keyframe; clip snap points use this.
    pub first_keyframe_100ns: i64,
    /// Size on disk or in RAM. Provisional while the segment is open.
    pub bytes: u64,
    /// Backing file path.
    pub path: PathBuf,
    /// False while the segment is the open (still-writing) segment.
    pub committed: bool,
}

/// A time range used to take a lease over the segments that overlap it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Inclusive start, 100 ns ticks.
    pub start_100ns: i64,
    /// Exclusive end, 100 ns ticks.
    pub end_100ns: i64,
}

/// A lease over a set of segments. While it is outstanding, those segments
/// are never evicted. Hand it back to [`RingIndex::release`]. It is not
/// `Clone`, so it can be released only once.
#[derive(Debug, PartialEq, Eq)]
pub struct Lease {
    seqs: Vec<u64>,
}

impl Lease {
    /// Sequence numbers covered by this lease, ascending.
    pub fn seqs(&self) -> &[u64] {
        &self.seqs
    }
}

/// The result of planning a clip from the ring.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPlan {
    /// Ordered segments to read, starting at the keyframe-aligned origin and
    /// running through the newest segment, including an open one.
    pub segments: Vec<SegmentMeta>,
    /// Ticks to skip from the start of `segments[0]` to reach the clip start.
    pub trim_start_offset_100ns: i64,
    /// Length of the clip after trimming, in seconds.
    pub effective_secs: f64,
    /// True when the buffer holds less than the requested duration.
    pub short: bool,
    /// True when the request reached back past a recorded gap, so the clip
    /// was clamped to the gap boundary.
    pub has_gap: bool,
}

/// Errors from [`RingIndex::plan_clip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipPlanError {
    /// No segments are available after the latest gap.
    Empty,
    /// `seconds` is zero or too large, or `pad_100ns` is negative.
    InvalidDuration,
}

impl fmt::Display for ClipPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "buffer holds no segments to clip"),
            Self::InvalidDuration => write!(f, "invalid clip duration or padding"),
        }
    }
}

impl std::error::Error for ClipPlanError {}

/// Errors from mutating operations on [`RingIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexError {
    /// No segment with this sequence number exists.
    UnknownSeq(u64),
    /// The segment was already committed.
    AlreadyCommitted(u64),
    /// The previous segment is still open, so a new one cannot be pushed.
    OpenSegmentPending(u64),
    /// The sequence number is not greater than the newest segment's.
    OutOfOrder(u64),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSeq(s) => write!(f, "unknown segment seq {s}"),
            Self::AlreadyCommitted(s) => write!(f, "segment seq {s} is already committed"),
            Self::OpenSegmentPending(s) => write!(f, "segment seq {s} is still open"),
            Self::OutOfOrder(s) => write!(f, "segment seq {s} is out of order"),
        }
    }
}

impl std::error::Error for IndexError {}

/// In-memory index of the segment ring, oldest first.
#[derive(Debug, Default)]
pub struct RingIndex {
    segments: VecDeque<SegmentMeta>,
    total_bytes: u64,
    /// Latest discontinuity. Clips never reach before it.
    gap: Option<i64>,
    lease_counts: HashMap<u64, u32>,
}

impl RingIndex {
    /// Creates an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks that a segment with sequence number `seq` may be appended.
    fn check_next(&self, seq: u64) -> Result<(), IndexError> {
        if let Some(back) = self.segments.back() {
            if !back.committed {
                return Err(IndexError::OpenSegmentPending(back.seq));
            }
            if seq <= back.seq {
                return Err(IndexError::OutOfOrder(seq));
            }
        }
        Ok(())
    }

    /// Appends a segment. The previous segment must be committed, and `seq`
    /// must be greater than the newest sequence number.
    ///
    /// The pushed segment stays open until [`RingIndex::commit`] is called, and
    /// later pushes fail with `OpenSegmentPending` until then. When the segment
    /// is already finished, prefer [`RingIndex::push_committed`], which cannot
    /// leave an open segment behind.
    pub fn push(&mut self, meta: SegmentMeta) -> Result<(), IndexError> {
        self.check_next(meta.seq)?;
        self.total_bytes = self.total_bytes.saturating_add(meta.bytes);
        self.segments.push_back(meta);
        Ok(())
    }

    /// Appends a segment that is already finished, in one step. It is stored
    /// committed, with `end_100ns` and `bytes` set. The validation rules are
    /// the same as [`RingIndex::push`].
    ///
    /// This is the preferred path when the segment is already finished. It
    /// never leaves an open segment, so the ring cannot refuse later pushes
    /// with `OpenSegmentPending`.
    pub fn push_committed(
        &mut self,
        mut meta: SegmentMeta,
        end_100ns: i64,
        bytes: u64,
    ) -> Result<(), IndexError> {
        self.check_next(meta.seq)?;
        meta.end_100ns = end_100ns;
        meta.bytes = bytes;
        meta.committed = true;
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.segments.push_back(meta);
        Ok(())
    }

    /// Seals an open segment with its final end time and byte count.
    pub fn commit(&mut self, seq: u64, end_100ns: i64, bytes: u64) -> Result<(), IndexError> {
        let idx = self
            .segments
            .binary_search_by_key(&seq, |s| s.seq)
            .map_err(|_| IndexError::UnknownSeq(seq))?;
        let seg = &mut self.segments[idx];
        if seg.committed {
            return Err(IndexError::AlreadyCommitted(seq));
        }
        self.total_bytes = self
            .total_bytes
            .saturating_sub(seg.bytes)
            .saturating_add(bytes);
        seg.end_100ns = end_100ns;
        seg.bytes = bytes;
        seg.committed = true;
        Ok(())
    }

    /// Records a discontinuity at `at_100ns`. Clips will not reach before the
    /// latest recorded gap.
    pub fn mark_gap(&mut self, at_100ns: i64) {
        self.gap = Some(match self.gap {
            Some(g) => g.max(at_100ns),
            None => at_100ns,
        });
    }

    /// Latest recorded gap boundary, if any.
    pub fn gap_100ns(&self) -> Option<i64> {
        self.gap
    }

    /// Evicts the oldest segments until the ring fits both limits.
    ///
    /// The duration limit allows one segment of slack, so a full-length clip
    /// can still snap back to a keyframe. Eviction stops at the first segment
    /// that is open or leased, which keeps the ring contiguous. Returned
    /// segments are oldest first; the caller deletes their backing files.
    pub fn evict_to(&mut self, max_duration_100ns: i64, max_bytes: u64) -> Vec<SegmentMeta> {
        let mut evicted = Vec::new();
        while let Some(oldest) = self.segments.front() {
            if !oldest.committed || self.is_leased(oldest.seq) {
                break;
            }
            let newest_end = self
                .segments
                .back()
                .map_or(oldest.end_100ns, |s| s.end_100ns);
            let span = newest_end.saturating_sub(oldest.start_100ns);
            let seg_dur = oldest.end_100ns.saturating_sub(oldest.start_100ns);
            let over_time = span > max_duration_100ns.saturating_add(seg_dur);
            let over_bytes = self.total_bytes > max_bytes;
            if !(over_time || over_bytes) {
                break;
            }
            if let Some(seg) = self.segments.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(seg.bytes);
                evicted.push(seg);
            }
        }
        evicted
    }

    /// Leases every segment that overlaps `window`. Those segments are not
    /// evicted until the lease is released.
    pub fn lease(&mut self, window: Window) -> Lease {
        let seqs: Vec<u64> = self
            .segments
            .iter()
            .filter(|s| s.end_100ns > window.start_100ns && s.start_100ns < window.end_100ns)
            .map(|s| s.seq)
            .collect();
        for seq in &seqs {
            *self.lease_counts.entry(*seq).or_insert(0) += 1;
        }
        Lease { seqs }
    }

    /// Releases a lease. Each covered segment's count drops by one.
    pub fn release(&mut self, lease: Lease) {
        for seq in lease.seqs {
            let drop_entry = match self.lease_counts.get_mut(&seq) {
                Some(count) => {
                    *count = count.saturating_sub(1);
                    *count == 0
                }
                None => false,
            };
            if drop_entry {
                self.lease_counts.remove(&seq);
            }
        }
    }

    /// Plans a clip of the last `seconds` seconds, ending at `now_100ns`.
    ///
    /// The target start is `now - seconds - pad`, clamped to the latest gap.
    /// The clip starts at the last segment whose first keyframe is at or
    /// before the target. If none is, it starts at the earliest available
    /// keyframe.
    pub fn plan_clip(
        &self,
        now_100ns: i64,
        seconds: u64,
        pad_100ns: i64,
    ) -> Result<ClipPlan, ClipPlanError> {
        if seconds == 0 || pad_100ns < 0 {
            return Err(ClipPlanError::InvalidDuration);
        }
        let want = i64::try_from(seconds)
            .ok()
            .and_then(|s| s.checked_mul(TICKS_PER_SEC))
            .ok_or(ClipPlanError::InvalidDuration)?;

        let lower = self.gap.unwrap_or(i64::MIN);
        let raw_target = now_100ns.saturating_sub(want).saturating_sub(pad_100ns);
        let has_gap = self.gap.is_some() && raw_target < lower;
        let target = raw_target.max(lower);

        let first = self
            .segments
            .iter()
            .position(|s| s.start_100ns >= lower)
            .ok_or(ClipPlanError::Empty)?;

        let earliest_kf = self.segments[first].first_keyframe_100ns;
        let short = now_100ns.saturating_sub(earliest_kf) < want;

        let k_idx = (first..self.segments.len())
            .rev()
            .find(|&i| self.segments[i].first_keyframe_100ns <= target)
            .unwrap_or(first);
        let origin = self.segments[k_idx].first_keyframe_100ns;
        let start = origin.max(target);
        let trim_start_offset_100ns = start - origin;
        let effective_secs = now_100ns.saturating_sub(start).max(0) as f64 / TICKS_PER_SEC as f64;

        Ok(ClipPlan {
            segments: self.segments.range(k_idx..).cloned().collect(),
            trim_start_offset_100ns,
            effective_secs,
            short,
            has_gap,
        })
    }

    /// Newest committed segment's end on the index timeline, if any.
    pub fn timeline_end_100ns(&self) -> Option<i64> {
        self.segments
            .iter()
            .rev()
            .find(|s| s.committed)
            .map(|s| s.end_100ns)
    }

    /// Plans a clip of the last `seconds` seconds of *recorded* footage.
    ///
    /// The index timeline is the sum of probed segment durations. It runs
    /// behind the wall clock whenever the encoder produces less media than
    /// real time (dropped or unpaced frames), and it always trails the wall
    /// clock by the open segment. Anchoring the window to `wall_now_100ns`
    /// then asks for footage that does not exist and the clip collapses to
    /// the newest segment. The window therefore ends at the newest committed
    /// segment's end, or at `wall_now_100ns` if that is earlier. Returns the
    /// plan and the anchor that was used.
    pub fn plan_latest_clip(
        &self,
        wall_now_100ns: i64,
        seconds: u64,
        pad_100ns: i64,
    ) -> Result<(ClipPlan, i64), ClipPlanError> {
        let anchor = self
            .timeline_end_100ns()
            .map_or(wall_now_100ns, |end| end.min(wall_now_100ns));
        self.plan_clip(anchor, seconds, pad_100ns)
            .map(|p| (p, anchor))
    }

    /// Number of segments in the index.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// True when the index holds no segments.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Sum of segment bytes in the index.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Iterates over segments, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &SegmentMeta> {
        self.segments.iter()
    }

    /// Oldest segment, in O(1). `None` when the index is empty.
    pub fn first(&self) -> Option<&SegmentMeta> {
        self.segments.front()
    }

    /// Newest segment, in O(1). `None` when the index is empty.
    pub fn last(&self) -> Option<&SegmentMeta> {
        self.segments.back()
    }

    fn is_leased(&self, seq: u64) -> bool {
        self.lease_counts.get(&seq).copied().unwrap_or(0) > 0
    }
}
