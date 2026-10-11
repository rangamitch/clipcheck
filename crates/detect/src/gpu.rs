//! GPU 3D utilization per process via PDH (task SIG2).
//!
//! The Windows counter `\GPU Engine(*)\Utilization Percentage` exposes one
//! instance per (process, adapter, engine) with names like
//! `pid_12345_luid_0x00000000_0x0000F3A1_phys_0_eng_0_engtype_3D`. Summing the
//! `3D` engine instances per pid gives a per-process 3D load. Only performance
//! counters are read: no process handles, no injection, no elevation.
//!
//! [`parse_instance`] and [`GpuTracker`] are pure. [`GpuMonitor`] is the
//! Windows-only PDH wrapper.

use std::collections::{HashMap, VecDeque};
use std::fmt;

/// Retention for [`GpuTracker`] samples and pid bookkeeping (ms).
const RETENTION_MS: u64 = 60_000;

/// One parsed `\GPU Engine` instance name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInstance {
    /// Owning process id.
    pub pid: u32,
    /// Engine type, e.g. `"3D"`, `"Compute"`, `"VideoDecode"`, `"Copy"`.
    /// Empty when the name carries no `engtype_` segment.
    pub engtype: String,
}

/// Errors from the PDH layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    /// A PDH call returned a non-zero status.
    Pdh {
        /// Name of the PDH function that failed.
        call: &'static str,
        /// Raw PDH status code.
        status: u32,
    },
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::Pdh { call, status } => {
                write!(f, "{call} failed with PDH status 0x{status:08X}")
            }
        }
    }
}

impl std::error::Error for GpuError {}

/// Parses a `\GPU Engine` instance name.
///
/// Returns `None` when no leading `pid_<digits>` segment exists or the pid
/// does not fit in `u32`. The `engtype` is the text after the first
/// `engtype_` up to the next `_` (or end of name); it is empty when absent.
/// Matching is ASCII case-insensitive for the `pid_` and `engtype_` markers.
pub fn parse_instance(name: &str) -> Option<GpuInstance> {
    let name = name.trim();
    let head = name.get(..4)?;
    if !head.eq_ignore_ascii_case("pid_") {
        return None;
    }
    let rest = &name[4..];
    let digits_end = rest.find('_').unwrap_or(rest.len());
    let pid: u32 = rest[..digits_end].parse().ok()?;

    let lower = name.to_ascii_lowercase();
    let engtype = match lower.find("engtype_") {
        Some(idx) => {
            let start = idx + "engtype_".len();
            let tail = name.get(start..).unwrap_or("");
            let end = tail.find('_').unwrap_or(tail.len());
            tail[..end].to_string()
        }
        None => String::new(),
    };

    Some(GpuInstance { pid, engtype })
}

/// Rolling per-pid window of 3D utilization samples.
///
/// Feed it with [`GpuTracker::push`] every poll; query with
/// [`GpuTracker::avg`]. Only pids present in a pushed map record a sample;
/// absent pids are not zero-filled. Samples and pids unseen for 60 s are
/// dropped on each push.
#[derive(Debug, Clone, Default)]
pub struct GpuTracker {
    /// Per-pid `(now_ms, percent)` samples, oldest first.
    samples: HashMap<u32, VecDeque<(u64, f32)>>,
    /// Last `now_ms` at which each pid appeared in a push.
    last_seen: HashMap<u32, u64>,
}

impl GpuTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one sample map taken at `now_ms` and prunes old state.
    pub fn push(&mut self, now_ms: u64, samples: &HashMap<u32, f32>) {
        let cutoff = now_ms.saturating_sub(RETENTION_MS);
        for (&pid, &value) in samples {
            if !value.is_finite() {
                continue;
            }
            let value = value.clamp(0.0, 100.0);
            self.samples
                .entry(pid)
                .or_default()
                .push_back((now_ms, value));
            self.last_seen.insert(pid, now_ms);
        }

        self.last_seen.retain(|_, &mut seen| seen >= cutoff);
        let last_seen = &self.last_seen;
        self.samples.retain(|pid, deque| {
            while deque.front().is_some_and(|&(t, _)| t < cutoff) {
                deque.pop_front();
            }
            last_seen.contains_key(pid) && !deque.is_empty()
        });
    }

    /// Mean of samples for `pid` taken within the last `window_ms` ms of
    /// `now_ms`. Uses whatever samples exist in that window (partial windows
    /// are averaged as-is). `None` when the pid has no samples in the window.
    pub fn avg(&self, pid: u32, window_ms: u64, now_ms: u64) -> Option<f32> {
        let deque = self.samples.get(&pid)?;
        let from = now_ms.saturating_sub(window_ms);
        let (sum, n) = deque
            .iter()
            .filter(|&&(t, _)| t >= from && t <= now_ms)
            .fold((0.0f64, 0usize), |(s, n), &(_, v)| {
                (s + f64::from(v), n + 1)
            });
        if n == 0 {
            None
        } else {
            Some((sum / n as f64) as f32)
        }
    }
}

#[cfg(windows)]
pub use win::GpuMonitor;

#[cfg(windows)]
mod win {
    use std::collections::HashMap;

    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
        PDH_MORE_DATA,
    };

    use super::{parse_instance, GpuError};

    /// English counter path; the wildcard expands to every GPU engine instance.
    const COUNTER_PATH: PCWSTR = w!("\\GPU Engine(*)\\Utilization Percentage");

    /// One counter item read from PDH, with the name decoded.
    struct RawItem {
        name: String,
        status: u32,
        value: f64,
    }

    /// Owns a PDH query on `\GPU Engine(*)\Utilization Percentage`.
    #[derive(Debug)]
    pub struct GpuMonitor {
        query: isize,
        counter: isize,
    }

    fn check(status: u32, call: &'static str) -> Result<(), GpuError> {
        if status == 0 {
            Ok(())
        } else {
            Err(GpuError::Pdh { call, status })
        }
    }

    impl GpuMonitor {
        /// Opens the query, adds the wildcard counter and primes it with one
        /// collection. Fails when the GPU Engine counters are unavailable.
        pub fn new() -> Result<Self, GpuError> {
            let mut query: isize = 0;
            // SAFETY: a null data source selects live data; `query` is a valid
            // out-pointer that PDH writes on success.
            let st = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
            check(st, "PdhOpenQueryW")?;

            let mut counter: isize = 0;
            // SAFETY: `query` is a live handle from PdhOpenQueryW; COUNTER_PATH
            // is a static NUL-terminated wide string; `counter` is a valid out-pointer.
            let st = unsafe { PdhAddEnglishCounterW(query, COUNTER_PATH, 0, &mut counter) };
            if let Err(e) = check(st, "PdhAddEnglishCounterW") {
                // SAFETY: `query` was opened above and is not used after this.
                unsafe { PdhCloseQuery(query) };
                return Err(e);
            }

            let monitor = GpuMonitor { query, counter };
            // SAFETY: `monitor.query` is a live handle. On error, `monitor` is
            // dropped here, which closes the query.
            let st = unsafe { PdhCollectQueryData(monitor.query) };
            check(st, "PdhCollectQueryData")?;
            Ok(monitor)
        }

        /// Collects a new sample and returns summed 3D utilization per pid
        /// (capped at 100). Items without valid data are skipped.
        pub fn sample(&mut self) -> Result<HashMap<u32, f32>, GpuError> {
            // SAFETY: `self.query` is a live handle owned by this monitor.
            let st = unsafe { PdhCollectQueryData(self.query) };
            check(st, "PdhCollectQueryData")?;

            let items = self.read_items()?;
            let mut out: HashMap<u32, f32> = HashMap::new();
            for item in items {
                if item.status != PDH_CSTATUS_VALID_DATA {
                    continue;
                }
                let Some(inst) = parse_instance(&item.name) else {
                    continue;
                };
                if !inst.engtype.eq_ignore_ascii_case("3D") {
                    continue;
                }
                if !item.value.is_finite() || item.value < 0.0 {
                    continue;
                }
                let entry = out.entry(inst.pid).or_insert(0.0);
                *entry = (*entry + item.value as f32).min(100.0);
            }
            Ok(out)
        }

        /// Reads all counter items using the two-call sizing pattern.
        fn read_items(&self) -> Result<Vec<RawItem>, GpuError> {
            const CALL: &str = "PdhGetFormattedCounterArrayW";
            const ATTEMPTS: usize = 4;
            let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let mut buf_size: u32 = 0;
            let mut count: u32 = 0;

            for _ in 0..ATTEMPTS {
                // SAFETY: sizing call with a null buffer; PDH only writes
                // `buf_size` and `count`, which are valid out-pointers.
                let st = unsafe {
                    PdhGetFormattedCounterArrayW(
                        self.counter,
                        PDH_FMT_DOUBLE,
                        &mut buf_size,
                        &mut count,
                        None,
                    )
                };
                if st == 0 {
                    return Ok(Vec::new());
                }
                if st != PDH_MORE_DATA {
                    return Err(GpuError::Pdh {
                        call: CALL,
                        status: st,
                    });
                }

                // Allocate at least `buf_size` bytes of items; PDH stores the
                // name strings after the item array in the same buffer.
                let bytes = buf_size as usize;
                let n = (bytes + item_size - 1) / item_size.max(1);
                let n = n.max(1);
                let mut buf = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); n];

                // SAFETY: `buf` holds `n * item_size >= buf_size` bytes, which is
                // the size PDH was told; `buf_size` is updated by PDH to the
                // bytes actually needed, and `count` to the item count.
                let st = unsafe {
                    PdhGetFormattedCounterArrayW(
                        self.counter,
                        PDH_FMT_DOUBLE,
                        &mut buf_size,
                        &mut count,
                        Some(buf.as_mut_ptr()),
                    )
                };
                if st == PDH_MORE_DATA {
                    continue;
                }
                check(st, CALL)?;

                let k = (count as usize).min(n);
                let mut out = Vec::with_capacity(k);
                for item in &buf[..k] {
                    let name = if item.szName.is_null() {
                        String::new()
                    } else {
                        // SAFETY: PDH guarantees `szName` points at a NUL-terminated
                        // wide string inside `buf`, which is still alive here.
                        unsafe { item.szName.to_string() }.unwrap_or_default()
                    };
                    // SAFETY: the value is requested with PDH_FMT_DOUBLE, so the
                    // `doubleValue` member of the union is the active one.
                    let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                    out.push(RawItem {
                        name,
                        status: item.FmtValue.CStatus,
                        value,
                    });
                }
                return Ok(out);
            }
            Err(GpuError::Pdh {
                call: CALL,
                status: PDH_MORE_DATA,
            })
        }
    }

    impl Drop for GpuMonitor {
        fn drop(&mut self) {
            if self.query != 0 {
                // SAFETY: `query` was opened by PdhOpenQueryW and is closed once.
                unsafe { PdhCloseQuery(self.query) };
                self.query = 0;
            }
        }
    }
}
