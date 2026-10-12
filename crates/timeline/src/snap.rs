//! Snapping. Pure functions over snap points; the UI chooses thresholds.

use std::collections::HashSet;

use crate::model::Project;
use crate::time::Tick;

/// What a snap point came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapKind {
    /// Timeline start (0).
    Origin,
    /// The playhead.
    Playhead,
    /// A clip's start or end.
    ClipEdge,
    /// A marker.
    Marker,
    /// Work-area in or out.
    WorkArea,
}

/// A candidate snap position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapPoint {
    /// Time.
    pub time: Tick,
    /// Source of the point.
    pub kind: SnapKind,
}

/// Stable ordering key for a kind (used to sort and de-duplicate points).
fn kind_rank(kind: SnapKind) -> u8 {
    match kind {
        SnapKind::Origin => 0,
        SnapKind::Playhead => 1,
        SnapKind::ClipEdge => 2,
        SnapKind::Marker => 3,
        SnapKind::WorkArea => 4,
    }
}

/// All snap points: origin, playhead, every enabled clip's start and end except clips in
/// `exclude`, markers, work-area edges. Sorted by time, duplicates (same time and kind) removed.
pub fn snap_points(p: &Project, exclude: &[String], playhead: Tick) -> Vec<SnapPoint> {
    let skip: HashSet<&str> = exclude.iter().map(String::as_str).collect();
    let mut out = vec![
        SnapPoint {
            time: Tick::ZERO,
            kind: SnapKind::Origin,
        },
        SnapPoint {
            time: playhead,
            kind: SnapKind::Playhead,
        },
    ];
    for c in &p.clips {
        if !c.enabled || skip.contains(c.id.as_str()) {
            continue;
        }
        out.push(SnapPoint {
            time: c.start,
            kind: SnapKind::ClipEdge,
        });
        out.push(SnapPoint {
            time: c.end(),
            kind: SnapKind::ClipEdge,
        });
    }
    for m in &p.markers {
        out.push(SnapPoint {
            time: m.time,
            kind: SnapKind::Marker,
        });
    }
    if let Some(wa) = p.work_area {
        out.push(SnapPoint {
            time: wa.start,
            kind: SnapKind::WorkArea,
        });
        out.push(SnapPoint {
            time: wa.end,
            kind: SnapKind::WorkArea,
        });
    }
    out.sort_by_key(|sp| (sp.time, kind_rank(sp.kind)));
    out.dedup_by(|a, b| a.time == b.time && a.kind == b.kind);
    out
}

/// Nearest point to `t` within `threshold` (inclusive) with its distance. Equal distances prefer
/// the earlier time.
fn nearest(t: Tick, points: &[SnapPoint], threshold: Tick) -> Option<(i128, SnapPoint)> {
    let limit = i128::from(threshold.0);
    let mut best: Option<(i128, SnapPoint)> = None;
    for pt in points {
        let d = (i128::from(pt.time.0) - i128::from(t.0)).abs();
        if d > limit {
            continue;
        }
        let better = match best {
            None => true,
            Some((bd, bp)) => d < bd || (d == bd && pt.time < bp.time),
        };
        if better {
            best = Some((d, *pt));
        }
    }
    best
}

/// Snaps `t` to the nearest point within `threshold` ticks (inclusive). None when nothing is close.
/// Equal distances prefer the earlier point.
pub fn snap_time(t: Tick, points: &[SnapPoint], threshold: Tick) -> Option<SnapPoint> {
    nearest(t, points, threshold).map(|(_, sp)| sp)
}

/// Snaps a moving block: given the block's start and end, returns the (delta to apply, point that
/// was hit) for whichever edge is closer to a point within `threshold`; None if neither edge snaps.
/// On equal distances the start edge wins.
pub fn snap_block(
    start: Tick,
    end: Tick,
    points: &[SnapPoint],
    threshold: Tick,
) -> Option<(Tick, SnapPoint)> {
    let s = nearest(start, points, threshold);
    let e = nearest(end, points, threshold);
    let (edge, hit) = match (s, e) {
        (None, None) => return None,
        (Some(sv), None) => (start, sv),
        (None, Some(ev)) => (end, ev),
        (Some(sv), Some(ev)) => {
            if ev.0 < sv.0 {
                (end, ev)
            } else {
                (start, sv)
            }
        }
    };
    let (_, sp) = hit;
    Some((sp.time - edge, sp))
}
