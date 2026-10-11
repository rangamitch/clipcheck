//! Helpers of the worker's scan steps: event publishing, probe plans and path matching.

use clipcheck_engine::scan::ScanPlan;
use clipcheck_ipc::proto::Event;

use crate::events::EventBus;

use super::{Item, Totals};

/// Publishes one `library_scan` event.
pub(super) fn publish_scan(
    bus: &EventBus,
    root_id: i64,
    path: &str,
    phase: &str,
    done: u64,
    total: u64,
    totals: &Totals,
) {
    bus.publish(Event::LibraryScan {
        root_id,
        path: path.to_owned(),
        phase: phase.to_owned(),
        done,
        total,
        added: totals.added,
        missing: totals.missing,
    });
}

/// Probe plan for one chunk of items.
pub(super) fn chunk_plan(chunk: Vec<Item>) -> ScanPlan {
    let mut plan = ScanPlan::default();
    for item in chunk {
        match item {
            Item::New(file) => plan.new.push(file),
            Item::Changed(id, file) => plan.changed.push((id, file)),
        }
    }
    plan
}

/// True when `rel` is `prefix` or lies below it.
pub(super) fn path_under(rel: &str, prefix: &str) -> bool {
    rel == prefix
        || rel
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(['\\', '/']))
}
