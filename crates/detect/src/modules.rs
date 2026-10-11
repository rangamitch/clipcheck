//! Loaded graphics modules per process (task SIG3).
//!
//! Read-only probe: lists the DLLs loaded in a candidate process and reports
//! which graphics APIs are present. Never reads module memory, never writes,
//! never injects. Protected or anti-cheat processes refuse the open and yield
//! [`ModuleProbe::Denied`], which is cached so they are not hammered each poll.
//!
//! The pure parts ([`classify`], [`ModuleCache`]) compile on every platform.
//! The Win32 parts are `cfg(windows)`.

use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use windows::core::{Error as WinError, HRESULT, PWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, HMODULE};
#[cfg(windows)]
use windows::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_ALL,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_ACCESS_RIGHTS,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

/// Graphics API DLLs that count as evidence of a rendering game.
///
/// `dxgi.dll` is deliberately excluded: too many non-game apps load it.
pub const GRAPHICS_DLLS: &[&str] = &[
    "d3d9.dll",
    "d3d10.dll",
    "d3d10_1.dll",
    "d3d11.dll",
    "d3d12.dll",
    "d3d12core.dll",
    "vulkan-1.dll",
    "opengl32.dll",
];

/// Result of probing one process for loaded graphics modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleProbe {
    /// At least one graphics DLL is loaded. Holds the lowercase DLL names,
    /// sorted and de-duplicated.
    Found(Vec<String>),
    /// The module list was read and contains no graphics DLL.
    NoneLoaded,
    /// The process could not be opened with the allowed rights (protected or
    /// anti-cheat process, or access denied).
    Denied,
    /// The process no longer exists (or has exited).
    Gone,
}

/// Returns the lowercase graphics DLL names among `module_base_names`.
///
/// Matching is case-insensitive. Each name appears once. The result is sorted.
/// Names not in [`GRAPHICS_DLLS`] (including `dxgi.dll`) are ignored.
pub fn classify(module_base_names: &[String]) -> Vec<String> {
    let found: BTreeSet<String> = module_base_names
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .filter(|n| GRAPHICS_DLLS.contains(&n.as_str()))
        .collect();
    found.into_iter().collect()
}

/// Maps an `OpenProcess` failure to a probe result.
#[cfg(windows)]
fn map_open_error(err: &WinError) -> ModuleProbe {
    if err.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) {
        ModuleProbe::Gone
    } else {
        // Access denied (protected/anti-cheat) or anything unexpected: quiet denial.
        ModuleProbe::Denied
    }
}

/// Owns a process handle and closes it exactly once on drop.
#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    /// Opens `pid` with the requested rights.
    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<Self> {
        // SAFETY: OpenProcess takes plain values and has no memory-safety
        // preconditions. The returned handle is wrapped so it is closed once.
        unsafe { OpenProcess(access, false, pid) }.map(OwnedHandle)
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful OpenProcess and is closed
        // only here, in Drop, so it is never used after this call.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Probes process `pid` for loaded graphics modules.
///
/// Opens the process with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`
/// only. Uses `LIST_MODULES_ALL` so 32-bit modules in WOW64 processes are seen.
#[cfg(windows)]
pub fn graphics_modules(pid: u32) -> ModuleProbe {
    let handle = match OwnedHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ) {
        Ok(h) => h,
        Err(e) => return map_open_error(&e),
    };

    // A handle can outlive the process. Treat an exited process as gone.
    let mut exit_code: u32 = 0;
    // SAFETY: `handle.0` is a valid handle with limited-information access, and
    // `exit_code` is a valid out pointer for the duration of the call.
    let exit_ok = unsafe { GetExitCodeProcess(handle.0, &mut exit_code) }.is_ok();
    if !exit_ok {
        return ModuleProbe::Denied;
    }
    const STILL_ACTIVE: u32 = 259;
    if exit_code != STILL_ACTIVE {
        return ModuleProbe::Gone;
    }

    let names = match module_base_names(handle.0) {
        Some(names) => names,
        None => return ModuleProbe::Denied,
    };
    match classify(&names) {
        found if found.is_empty() => ModuleProbe::NoneLoaded,
        found => ModuleProbe::Found(found),
    }
}

/// Reads the base names of all modules loaded in `process`.
///
/// Starts with a small buffer and grows it when the process reports more
/// modules than fit. Returns `None` if the module list cannot be read.
#[cfg(windows)]
fn module_base_names(process: HANDLE) -> Option<Vec<String>> {
    const MAX_ATTEMPTS: usize = 4;
    let module_size = std::mem::size_of::<HMODULE>();
    let mut modules: Vec<HMODULE> = vec![HMODULE(std::ptr::null_mut()); 256];

    for _ in 0..MAX_ATTEMPTS {
        let cb = u32::try_from(modules.len() * module_size).ok()?;
        let mut needed: u32 = 0;
        // SAFETY: `process` is a valid handle with query and VM-read access. The
        // `modules` buffer has `cb` bytes of writable storage. `needed` is a valid
        // out pointer. Only the module handle list is written, not module memory.
        let ok = unsafe {
            EnumProcessModulesEx(
                process,
                modules.as_mut_ptr(),
                cb,
                &mut needed,
                LIST_MODULES_ALL,
            )
        }
        .is_ok();
        if !ok {
            return None;
        }
        let count = needed as usize / module_size;
        if needed <= cb {
            modules.truncate(count);
            return Some(
                modules
                    .iter()
                    .filter_map(|m| base_name(process, *m))
                    .collect(),
            );
        }
        // Buffer too small: `count` is the real total, so grow and retry.
        modules.resize(count, HMODULE(std::ptr::null_mut()));
    }
    None
}

/// Returns the base name of one module, or `None` if it cannot be read.
#[cfg(windows)]
fn base_name(process: HANDLE, module: HMODULE) -> Option<String> {
    let mut buf = [0u16; 260];
    // SAFETY: `process` is a valid handle with query and VM-read access, `module`
    // came from EnumProcessModulesEx for that process, and `buf` is writable
    // with the length passed implicitly by the slice.
    let len = unsafe { GetModuleBaseNameW(process, module, &mut buf) };
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Returns the full image path of process `pid`, or `None` if it cannot be read.
///
/// Uses `PROCESS_QUERY_LIMITED_INFORMATION` only.
#[cfg(windows)]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    const MAX_WIDE_LEN: usize = 32_768;
    let handle = OwnedHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buf = vec![0u16; 1024];
    loop {
        let mut size = u32::try_from(buf.len()).ok()?;
        // SAFETY: `handle.0` is valid with limited-information access. `buf` has
        // `size` writable u16 slots, and `size` is a valid in/out pointer.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        }
        .is_ok();
        if ok {
            return Some(PathBuf::from(OsString::from_wide(&buf[..size as usize])));
        }
        if buf.len() >= MAX_WIDE_LEN {
            return None;
        }
        buf.resize((buf.len() * 2).min(MAX_WIDE_LEN), 0);
    }
}

/// Re-check interval after a `Found` result.
const FOUND_RECHECK_MS: u64 = 30_000;
/// Fast re-check interval while a process has no graphics module yet.
const NONE_FAST_MS: u64 = 3_000;
/// How long the fast interval lasts after the first `NoneLoaded` result.
const NONE_FAST_WINDOW_MS: u64 = 60_000;
/// Slow re-check interval after the fast window.
const NONE_SLOW_MS: u64 = 15_000;
/// How long a `Denied` result is trusted.
const DENIED_RECHECK_MS: u64 = 300_000;

#[derive(Debug, Clone)]
struct Entry {
    probe: ModuleProbe,
    checked_ms: u64,
    /// Time of the first `NoneLoaded` in the current run of `NoneLoaded` results.
    none_since_ms: u64,
}

impl Entry {
    fn is_stale(&self, now_ms: u64) -> bool {
        let age = now_ms.saturating_sub(self.checked_ms);
        match &self.probe {
            ModuleProbe::Found(_) => age >= FOUND_RECHECK_MS,
            ModuleProbe::NoneLoaded => {
                let since = now_ms.saturating_sub(self.none_since_ms);
                let interval = if since < NONE_FAST_WINDOW_MS {
                    NONE_FAST_MS
                } else {
                    NONE_SLOW_MS
                };
                age >= interval
            }
            ModuleProbe::Denied => age >= DENIED_RECHECK_MS,
            // Gone is never served from cache: re-probe (cheap) and let
            // `forget_dead` drop the entry once the pid is no longer alive.
            ModuleProbe::Gone => true,
        }
    }
}

/// Per-pid cache of [`ModuleProbe`] results with rate limiting.
///
/// The probe function and the clock are injected: `get` takes `now_ms` from the
/// caller and the probe is any `FnMut(u32) -> ModuleProbe`. Tests use a stub
/// probe and a fake clock. Production code uses [`ModuleCache::new`], which
/// wraps [`graphics_modules`].
///
/// Re-check rules:
/// - `Found`: every 30 s.
/// - `NoneLoaded`: every 3 s for the first 60 s, then every 15 s.
/// - `Denied`: not re-checked for 5 min. Callers may still treat it as unknown.
/// - `Gone`: never cached. Re-probed on access, dropped by [`ModuleCache::forget_dead`].
#[derive(Debug)]
pub struct ModuleCache<P> {
    probe: P,
    entries: HashMap<u32, Entry>,
}

#[cfg(windows)]
impl ModuleCache<fn(u32) -> ModuleProbe> {
    /// Creates a cache that probes real processes with [`graphics_modules`].
    pub fn new() -> Self {
        Self::with_probe(graphics_modules as fn(u32) -> ModuleProbe)
    }
}

#[cfg(windows)]
impl Default for ModuleCache<fn(u32) -> ModuleProbe> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: FnMut(u32) -> ModuleProbe> ModuleCache<P> {
    /// Creates a cache that uses `probe` to inspect processes.
    pub fn with_probe(probe: P) -> Self {
        Self {
            probe,
            entries: HashMap::new(),
        }
    }

    /// Returns the probe result for `pid` at time `now_ms`, probing only when the
    /// cached value is stale under the re-check rules.
    pub fn get(&mut self, pid: u32, now_ms: u64) -> &ModuleProbe {
        let stale = self.entries.get(&pid).is_none_or(|e| e.is_stale(now_ms));
        if stale {
            let probe = (self.probe)(pid);
            let none_since_ms = match (self.entries.get(&pid), &probe) {
                (Some(prev), ModuleProbe::NoneLoaded) if prev.probe == ModuleProbe::NoneLoaded => {
                    prev.none_since_ms
                }
                _ => now_ms,
            };
            self.entries.insert(
                pid,
                Entry {
                    probe,
                    checked_ms: now_ms,
                    none_since_ms,
                },
            );
        }
        // The entry was just inserted if it was stale, so this lookup always succeeds.
        &self.entries[&pid].probe
    }

    /// Drops every cached entry whose pid is not in `alive_pids`.
    pub fn forget_dead(&mut self, alive_pids: &HashSet<u32>) {
        self.entries.retain(|pid, _| alive_pids.contains(pid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_filters_and_lowercases() {
        let input = vec![
            "KERNEL32.dll".to_string(),
            "D3D11.DLL".to_string(),
            "dxgi.dll".to_string(),
            "d3d11.dll".to_string(),
        ];
        assert_eq!(classify(&input), vec!["d3d11.dll".to_string()]);
    }
}
