//! Tests for `clipcheck_detect::modules`.
//!
//! Pure tests (`classify`, `ModuleCache` with a stub probe and a fake clock)
//! run on every platform. Live tests that touch Win32 are `cfg(windows)`.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use clipcheck_detect::modules::{classify, ModuleCache, ModuleProbe, GRAPHICS_DLLS};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// classify (pure)
// ---------------------------------------------------------------------------

#[test]
fn classify_is_case_insensitive() {
    assert_eq!(classify(&names(&["D3D11.DLL"])), names(&["d3d11.dll"]));
}

#[test]
fn classify_excludes_dxgi() {
    assert!(classify(&names(&["dxgi.dll", "DXGI.DLL"])).is_empty());
}

#[test]
fn classify_collapses_duplicates() {
    let input = names(&["d3d11.dll", "D3D11.dll", "d3d11.dll"]);
    assert_eq!(classify(&input), names(&["d3d11.dll"]));
}

#[test]
fn classify_empty_input_is_empty() {
    assert!(classify(&[]).is_empty());
}

#[test]
fn classify_ignores_non_graphics_modules() {
    let input = names(&["kernel32.dll", "ntdll.dll", "user32.dll"]);
    assert!(classify(&input).is_empty());
}

#[test]
fn classify_returns_multiple_sorted() {
    let input = names(&["vulkan-1.dll", "opengl32.dll", "kernel32.dll", "d3d11.dll"]);
    assert_eq!(
        classify(&input),
        names(&["d3d11.dll", "opengl32.dll", "vulkan-1.dll"])
    );
}

#[test]
fn classify_recognizes_vulkan_and_d3d12core() {
    assert_eq!(
        classify(&names(&["Vulkan-1.dll"])),
        names(&["vulkan-1.dll"])
    );
    assert_eq!(
        classify(&names(&["D3D12Core.dll"])),
        names(&["d3d12core.dll"])
    );
}

#[test]
fn classify_recognizes_every_graphics_dll_on_itself() {
    for dll in GRAPHICS_DLLS {
        assert_eq!(
            classify(&names(&[dll])),
            names(&[dll]),
            "GRAPHICS_DLLS entry {dll} not recognized"
        );
    }
}

// ---------------------------------------------------------------------------
// ModuleCache (stub probe, fake clock)
// ---------------------------------------------------------------------------

/// Returns (scripted value, call counter, probe closure). The probe returns the
/// current scripted value for every pid and counts its calls.
type Scripted<F> = (Rc<RefCell<ModuleProbe>>, Rc<Cell<u32>>, F);

fn scripted(initial: ModuleProbe) -> Scripted<impl FnMut(u32) -> ModuleProbe> {
    let value = Rc::new(RefCell::new(initial));
    let calls = Rc::new(Cell::new(0u32));
    let (v, c) = (value.clone(), calls.clone());
    let probe = move |_pid: u32| {
        c.set(c.get() + 1);
        v.borrow().clone()
    };
    (value, calls, probe)
}

fn found_d3d11() -> ModuleProbe {
    ModuleProbe::Found(names(&["d3d11.dll"]))
}

#[test]
fn found_not_reprobed_before_30s() {
    let (_value, calls, probe) = scripted(found_d3d11());
    let mut cache = ModuleCache::with_probe(probe);
    assert_eq!(cache.get(1, 0).clone(), found_d3d11());
    assert_eq!(cache.get(1, 29_999).clone(), found_d3d11());
    assert_eq!(calls.get(), 1, "Found re-probed before 30 s");
}

#[test]
fn found_reprobed_at_30s() {
    let (_value, calls, probe) = scripted(found_d3d11());
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    cache.get(1, 30_000);
    assert_eq!(calls.get(), 2, "Found not re-probed at 30 s");
}

#[test]
fn none_loaded_not_reprobed_before_3s() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    cache.get(1, 2_999);
    assert_eq!(calls.get(), 1, "NoneLoaded re-probed before 3 s");
}

#[test]
fn none_loaded_reprobed_at_3s_within_window() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    cache.get(1, 3_000);
    assert_eq!(calls.get(), 2, "NoneLoaded not re-probed at 3 s");
}

#[test]
fn none_loaded_fast_interval_until_60s_boundary() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    // Probes every 3 s: 3, 6, ..., 57 s -> 19 re-probes plus the first.
    for t in (3_000..=57_000).step_by(3_000) {
        cache.get(1, t);
    }
    assert_eq!(calls.get(), 20);
    // At 60 s the first-NoneLoaded time is 0, so the window is over and the
    // slow 15 s interval applies. Age is only 3 s: no probe.
    cache.get(1, 60_000);
    assert_eq!(calls.get(), 20, "slow interval not applied at 60 s");
}

#[test]
fn none_loaded_fast_window_is_below_60s() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    // since = 59_999 < 60_000: still the fast 3 s rule, so this probes.
    cache.get(1, 59_999);
    assert_eq!(calls.get(), 2);
    // 62_999: age 3_000 but since >= 60 s, so the slow 15 s rule applies: no probe.
    cache.get(1, 62_999);
    assert_eq!(calls.get(), 2, "should be slow after the window");
    // 74_998: age 14_999 < 15 s: no probe.
    cache.get(1, 74_998);
    assert_eq!(calls.get(), 2);
    // 74_999 is age 15_000 from the probe at 59_999: probe.
    cache.get(1, 74_999);
    assert_eq!(calls.get(), 3, "slow 15 s interval not reached at 74_999");
}

#[test]
fn none_loaded_after_window_uses_15s_spacing() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    // Probe at 60 s: since = 60_000 is not < 60_000, so the slow 15 s rule
    // applies, and age 60 s >= 15 s, so this is a probe.
    cache.get(1, 60_000);
    assert_eq!(calls.get(), 2, "no probe at 60 s after first NoneLoaded");
    // checked at 60_000: 61_000 is age 1_000, not stale.
    cache.get(1, 61_000);
    assert_eq!(calls.get(), 2);
    // 74_999 is age 14_999: not stale.
    cache.get(1, 74_999);
    assert_eq!(calls.get(), 2, "re-probed before 15 s after slow switch");
    // 75_000 is age 15_000: stale.
    cache.get(1, 75_000);
    assert_eq!(calls.get(), 3, "not re-probed at 15 s spacing");
}

#[test]
fn none_loaded_window_is_preserved_across_consecutive_results() {
    let (_value, calls, probe) = scripted(ModuleProbe::NoneLoaded);
    let mut cache = ModuleCache::with_probe(probe);
    // Probes every 3 s from t=0 up to t=57 s; each probe is a NoneLoaded
    // result, so none_since must stay at 0 (not reset at every probe).
    for t in (0..=57_000).step_by(3_000) {
        cache.get(1, t);
    }
    let before = calls.get();
    // 60_000 - 57_000 = 3_000 age. If none_since were reset at 57_000 this
    // would be the fast window and would probe. Expected: no probe.
    cache.get(1, 60_000);
    assert_eq!(
        calls.get(),
        before,
        "none_since reset across consecutive NoneLoaded results"
    );
}

#[test]
fn denied_not_reprobed_before_300s() {
    let (_value, calls, probe) = scripted(ModuleProbe::Denied);
    let mut cache = ModuleCache::with_probe(probe);
    assert_eq!(cache.get(1, 0).clone(), ModuleProbe::Denied);
    assert_eq!(cache.get(1, 299_999).clone(), ModuleProbe::Denied);
    assert_eq!(calls.get(), 1, "Denied re-probed before 300 s");
}

#[test]
fn denied_reprobed_at_300s() {
    let (_value, calls, probe) = scripted(ModuleProbe::Denied);
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    cache.get(1, 300_000);
    assert_eq!(calls.get(), 2, "Denied not re-probed at 300 s");
}

#[test]
fn gone_is_reprobed_on_every_get() {
    let (_value, calls, probe) = scripted(ModuleProbe::Gone);
    let mut cache = ModuleCache::with_probe(probe);
    assert_eq!(cache.get(1, 0).clone(), ModuleProbe::Gone);
    assert_eq!(cache.get(1, 0).clone(), ModuleProbe::Gone);
    assert_eq!(cache.get(1, 1).clone(), ModuleProbe::Gone);
    assert_eq!(calls.get(), 3, "Gone was served from cache");
}

#[test]
fn forget_dead_drops_pid_so_next_get_reprobes() {
    let (_value, calls, probe) = scripted(found_d3d11());
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0);
    assert_eq!(calls.get(), 1);
    cache.forget_dead(&HashSet::new());
    cache.get(1, 1);
    assert_eq!(calls.get(), 2, "dead pid still served from cache");
}

#[test]
fn forget_dead_keeps_alive_pid_cached() {
    let (_value, calls, probe) = scripted(found_d3d11());
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(2, 0);
    let alive: HashSet<u32> = [2u32].into_iter().collect();
    cache.forget_dead(&alive);
    cache.get(2, 100);
    assert_eq!(calls.get(), 1, "alive pid was dropped from cache");
}

#[test]
fn found_then_none_loaded_restarts_none_window() {
    let (value, calls, probe) = scripted(found_d3d11());
    let mut cache = ModuleCache::with_probe(probe);
    cache.get(1, 0); // Found, probe 1
    *value.borrow_mut() = ModuleProbe::NoneLoaded;
    // Found is stale at 30 s: probe 2 returns NoneLoaded, none_since = 100 s.
    assert_eq!(cache.get(1, 100_000).clone(), ModuleProbe::NoneLoaded);
    assert_eq!(calls.get(), 2);
    // A fresh none window starts at 100 s, so 103 s is fast-stale and must
    // probe. A stale none_since of 0 would wrongly use the 15 s interval.
    cache.get(1, 103_000);
    assert_eq!(
        calls.get(),
        3,
        "none window did not restart after Found -> NoneLoaded"
    );
}

#[test]
fn cache_returns_probe_value_from_stub() {
    let mut cache = ModuleCache::with_probe(|pid| {
        if pid == 7 {
            ModuleProbe::Denied
        } else {
            ModuleProbe::NoneLoaded
        }
    });
    assert_eq!(cache.get(7, 0).clone(), ModuleProbe::Denied);
    assert_eq!(cache.get(8, 0).clone(), ModuleProbe::NoneLoaded);
}

// ---------------------------------------------------------------------------
// Live Win32 tests
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod live {
    use super::*;
    use clipcheck_detect::modules::{exe_path, graphics_modules};
    use std::path::{Path, PathBuf};
    use windows::core::w;
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::LoadLibraryW;

    /// Canonical, lowercase string form with any `\\?\` prefix removed.
    fn norm(path: &Path) -> String {
        let canon: PathBuf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let s = canon.to_string_lossy().to_string();
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_lowercase()
    }

    #[test]
    fn live_loaded_d3d11_is_found() {
        // SAFETY: LoadLibraryW is given a literal system DLL name. The returned
        // module is passed to FreeLibrary exactly once below.
        let module = unsafe { LoadLibraryW(w!("d3d11.dll")) }.expect("LoadLibraryW(d3d11.dll)");
        let probe = graphics_modules(std::process::id());
        // SAFETY: `module` came from the successful LoadLibraryW above and is
        // freed exactly once here.
        unsafe { FreeLibrary(module) }.expect("FreeLibrary(d3d11.dll)");

        assert_ne!(probe, ModuleProbe::Denied, "own process probe was Denied");
        match probe {
            ModuleProbe::Found(v) => {
                assert!(v.iter().any(|n| n == "d3d11.dll"), "d3d11.dll not in {v:?}")
            }
            other => panic!("expected Found containing d3d11.dll, got {other:?}"),
        }
    }

    #[test]
    fn live_exe_path_is_own_exe() {
        let got = exe_path(std::process::id()).expect("exe_path(own pid) returned None");
        let want = std::env::current_exe().expect("current_exe");
        assert_eq!(norm(&got), norm(&want));
    }

    #[test]
    fn live_exited_child_is_gone() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .expect("spawn cmd /c exit");
        let pid = child.id();
        child.wait().expect("wait for cmd");
        assert_eq!(graphics_modules(pid), ModuleProbe::Gone);
        assert!(exe_path(pid).is_none(), "exe_path of exited child was Some");
    }

    #[test]
    fn live_pid_zero_and_four_do_not_panic() {
        for pid in [0u32, 4u32] {
            let probe = graphics_modules(pid);
            assert!(
                matches!(probe, ModuleProbe::Denied | ModuleProbe::Gone),
                "graphics_modules({pid}) = {probe:?}, expected Denied or Gone"
            );
            assert!(exe_path(pid).is_none(), "exe_path({pid}) was Some");
        }
    }
}
