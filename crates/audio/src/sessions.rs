//! Audio sessions: which processes are playing (or have played) audio on the
//! render endpoints, plus a pure tracker of every app seen playing audio.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::AudioError;

/// Playback state of an audio session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The session is currently playing or capturing audio.
    Active,
    /// The session exists but is idle.
    Inactive,
    /// The session has ended (the owning process released it).
    Expired,
}

/// One application's audio session, merged across render endpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSessionInfo {
    /// Process ID of the first session seen for this app (0 for system sounds).
    pub pid: u32,
    /// Executable file name, e.g. `Discord.exe`. Empty for system sounds.
    pub exe_name: String,
    /// Session display name, else the exe file description, else the exe stem.
    pub display_name: String,
    /// Full path of the executable, when it could be read.
    pub exe_path: Option<PathBuf>,
    /// Merged state: `Active` if any session is active.
    pub state: SessionState,
    /// Peak meter value across the app's sessions, 0..1.
    pub peak: f32,
    /// True for the Windows system-sounds session.
    pub is_system_sounds: bool,
}

/// Lists audio sessions on every active render endpoint, one row per app.
///
/// Sessions of the same executable are merged (max peak, any active wins).
/// The current process is excluded. Sessions whose executable cannot be
/// resolved are skipped; a failure on one endpoint does not fail the call.
#[cfg(windows)]
pub fn list_sessions() -> Result<Vec<AudioSessionInfo>, AudioError> {
    use crate::devices::{win, EndpointKind};

    let _com = win::com_scope();
    let own_pid = std::process::id();
    let mut raw = Vec::new();
    for device in win::active_devices(EndpointKind::Render)? {
        raw.extend(win_impl::device_sessions(&device, own_pid));
    }
    Ok(merge_sessions(raw))
}

/// Non-Windows stub: audio sessions need WASAPI.
#[cfg(not(windows))]
pub fn list_sessions() -> Result<Vec<AudioSessionInfo>, AudioError> {
    Err(AudioError::Unsupported("audio sessions require Windows"))
}

/// Key under which sessions merge: the exe name, or one shared key for system sounds.
fn session_key(s: &AudioSessionInfo) -> String {
    if s.is_system_sounds {
        "\u{1}system-sounds".to_owned()
    } else {
        s.exe_name.to_lowercase()
    }
}

/// Merges per-endpoint sessions into one row per app and sorts them by display name.
fn merge_sessions(raw: Vec<AudioSessionInfo>) -> Vec<AudioSessionInfo> {
    let mut map: HashMap<String, AudioSessionInfo> = HashMap::new();
    for s in raw {
        let key = session_key(&s);
        match map.get_mut(&key) {
            Some(m) => {
                m.peak = m.peak.max(s.peak);
                m.state = merge_state(m.state, s.state);
                if m.display_name.is_empty() {
                    m.display_name = s.display_name;
                }
                if m.exe_path.is_none() {
                    m.exe_path = s.exe_path;
                }
            }
            None => {
                map.insert(key, s);
            }
        }
    }
    let mut out: Vec<AudioSessionInfo> = map.into_values().collect();
    out.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.exe_name.cmp(&b.exe_name))
    });
    out
}

fn merge_state(a: SessionState, b: SessionState) -> SessionState {
    use SessionState::*;
    match (a, b) {
        (Active, _) | (_, Active) => Active,
        (Inactive, _) | (_, Inactive) => Inactive,
        _ => Expired,
    }
}

/// Clamps a meter value to 0..=1 and maps non-finite values to 0.
fn clamp_peak(p: f32) -> f32 {
    if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Tracks every app observed playing audio, with first and last seen times.
///
/// Pure bookkeeping with no OS calls. Keys are case-insensitive exe names.
#[derive(Clone, Debug, Default)]
pub struct SeenApps {
    apps: HashMap<String, SeenApp>,
}

/// One app remembered by [`SeenApps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenApp {
    /// Executable file name.
    pub exe_name: String,
    /// Last known display name (may be empty).
    pub display_name: String,
    /// Last known executable path.
    pub exe_path: Option<PathBuf>,
    /// Time (ms, caller's clock) the app was first seen playing audio.
    pub first_seen_ms: u64,
    /// Time (ms, caller's clock) the app was last seen playing audio.
    pub last_seen_ms: u64,
}

impl SeenApps {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records every session that is Active or has a peak above 0.001 at `now_ms`.
    /// Sessions without an exe name (system sounds) are ignored.
    pub fn observe(&mut self, infos: &[AudioSessionInfo], now_ms: u64) {
        for info in infos {
            if info.exe_name.is_empty() {
                continue;
            }
            if info.state != SessionState::Active && info.peak <= 0.001 {
                continue;
            }
            let entry = self
                .apps
                .entry(info.exe_name.to_lowercase())
                .or_insert_with(|| SeenApp {
                    exe_name: info.exe_name.clone(),
                    display_name: String::new(),
                    exe_path: None,
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                });
            entry.last_seen_ms = entry.last_seen_ms.max(now_ms);
            if !info.display_name.is_empty() {
                entry.display_name = info.display_name.clone();
            }
            if info.exe_path.is_some() {
                entry.exe_path = info.exe_path.clone();
            }
        }
    }

    /// Apps last seen playing audio within `max_age_ms` of `now_ms`, most recent first.
    /// Ties are ordered by exe name (case-insensitive).
    pub fn list(&self, now_ms: u64, max_age_ms: u64) -> Vec<SeenApp> {
        let mut out: Vec<SeenApp> = self
            .apps
            .values()
            .filter(|a| now_ms.saturating_sub(a.last_seen_ms) <= max_age_ms)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.last_seen_ms
                .cmp(&a.last_seen_ms)
                .then_with(|| a.exe_name.to_lowercase().cmp(&b.exe_name.to_lowercase()))
        });
        out
    }

    /// Removes an app from the tracker (case-insensitive exe name).
    pub fn forget(&mut self, exe: &str) {
        self.apps.remove(&exe.to_lowercase());
    }
}

/// Windows session enumeration helpers.
#[cfg(windows)]
mod win_impl {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;

    use windows::core::{Interface, PCWSTR, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionManager2, IMMDevice,
    };
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::System::Com::CLSCTX_ALL;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{clamp_peak, AudioSessionInfo, SessionState};
    use crate::devices::win::take_co_string;

    /// Display name for the system-sounds session.
    const SYSTEM_SOUNDS_NAME: &str = "System sounds";

    /// Reads every session on one render device. Failures on individual sessions skip them.
    pub(crate) fn device_sessions(device: &IMMDevice, own_pid: u32) -> Vec<AudioSessionInfo> {
        let mut out = Vec::new();
        // SAFETY: the device is a valid COM object; all pointers passed are local or null.
        unsafe {
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                return out;
            };
            let Ok(list) = manager.GetSessionEnumerator() else {
                return out;
            };
            let count = list.GetCount().unwrap_or(0);
            for i in 0..count {
                let Ok(ctl) = list.GetSession(i) else {
                    continue;
                };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let is_system = ctl2.IsSystemSoundsSession().0 == 0; // S_OK
                let pid = if is_system {
                    0
                } else {
                    match ctl2.GetProcessId() {
                        Ok(p) => p,
                        Err(_) => continue,
                    }
                };
                if !is_system && (pid == 0 || pid == own_pid) {
                    continue;
                }
                let state = match ctl2.GetState() {
                    Ok(s) if s == AudioSessionStateActive => SessionState::Active,
                    Ok(s) if s == AudioSessionStateExpired => SessionState::Expired,
                    _ => SessionState::Inactive,
                };
                let session_name = ctl2
                    .GetDisplayName()
                    .map(take_co_string)
                    .unwrap_or_default();
                let peak = ctl
                    .cast::<IAudioMeterInformation>()
                    .ok()
                    .and_then(|m| m.GetPeakValue().ok())
                    .map(clamp_peak)
                    .unwrap_or(0.0);

                if is_system {
                    let display_name = if session_name.is_empty() {
                        SYSTEM_SOUNDS_NAME.to_owned()
                    } else {
                        session_name
                    };
                    out.push(AudioSessionInfo {
                        pid: 0,
                        exe_name: String::new(),
                        display_name,
                        exe_path: None,
                        state,
                        peak,
                        is_system_sounds: true,
                    });
                    continue;
                }

                let Some(path) = exe_path_for_pid(pid) else {
                    continue;
                };
                let exe_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if exe_name.is_empty() {
                    continue;
                }
                let display_name = if !session_name.is_empty() {
                    session_name
                } else if let Some(desc) = file_description(&path) {
                    desc
                } else {
                    path.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| exe_name.clone())
                };
                out.push(AudioSessionInfo {
                    pid,
                    exe_name,
                    display_name,
                    exe_path: Some(path),
                    state,
                    peak,
                    is_system_sounds: false,
                });
            }
        }
        out
    }

    /// Full image path of a process, or `None` when it cannot be opened or queried.
    fn exe_path_for_pid(pid: u32) -> Option<PathBuf> {
        // SAFETY: the handle is opened, used for one query, and closed on every path below.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            if ok.is_err() || len == 0 {
                return None;
            }
            Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..len as usize],
            )))
        }
    }

    /// `FileDescription` from the executable's version resource, when present.
    fn file_description(path: &Path) -> Option<String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut handle = 0u32;
        // SAFETY: `wide` is NUL-terminated and outlives the calls; `block` is sized from the first call.
        unsafe {
            let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), Some(&mut handle));
            if size == 0 {
                return None;
            }
            let mut block = vec![0u8; size as usize];
            GetFileVersionInfoW(
                PCWSTR(wide.as_ptr()),
                0,
                size,
                block.as_mut_ptr() as *mut c_void,
            )
            .ok()?;

            let query = |sub: &str| -> Option<(*mut c_void, u32)> {
                let sub_w: Vec<u16> = sub.encode_utf16().chain(Some(0)).collect();
                let mut ptr: *mut c_void = null_mut();
                let mut len = 0u32;
                let ok = VerQueryValueW(
                    block.as_ptr() as *const c_void,
                    PCWSTR(sub_w.as_ptr()),
                    &mut ptr,
                    &mut len,
                );
                if !ok.as_bool() || ptr.is_null() || len == 0 {
                    None
                } else {
                    Some((ptr, len))
                }
            };

            // Language and code page of the first translation entry; en-US as a fallback.
            let (lang, cp) = match query("\\VarFileInfo\\Translation") {
                Some((ptr, len)) if len >= 4 => {
                    let p = ptr as *const u16;
                    (
                        std::ptr::read_unaligned(p),
                        std::ptr::read_unaligned(p.add(1)),
                    )
                }
                _ => (0x0409u16, 0x04b0u16),
            };
            let sub = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription");
            let text = query(&sub).or_else(|| query("\\StringFileInfo\\040904b0\\FileDescription"));
            let (ptr, _) = text?;
            let s = PCWSTR(ptr as *const u16).to_string().ok()?;
            (!s.trim().is_empty()).then(|| s.trim().to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(exe: &str, state: SessionState, peak: f32) -> AudioSessionInfo {
        AudioSessionInfo {
            pid: 1,
            exe_name: exe.into(),
            display_name: String::new(),
            exe_path: None,
            state,
            peak,
            is_system_sounds: false,
        }
    }

    #[test]
    fn merge_state_active_wins() {
        assert_eq!(
            merge_state(SessionState::Inactive, SessionState::Active),
            SessionState::Active
        );
        assert_eq!(
            merge_state(SessionState::Expired, SessionState::Inactive),
            SessionState::Inactive
        );
    }

    #[test]
    fn observe_skips_idle_and_nameless() {
        let mut seen = SeenApps::new();
        seen.observe(
            &[
                info("a.exe", SessionState::Inactive, 0.0),
                info("", SessionState::Active, 0.5),
            ],
            10,
        );
        assert!(seen.list(10, 1000).is_empty());
    }

    #[test]
    fn observe_tracks_first_and_last_seen() {
        let mut seen = SeenApps::new();
        seen.observe(&[info("A.exe", SessionState::Active, 0.0)], 100);
        seen.observe(&[info("a.exe", SessionState::Inactive, 0.2)], 300);
        let list = seen.list(300, 1000);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].first_seen_ms, 100);
        assert_eq!(list[0].last_seen_ms, 300);
    }

    #[test]
    fn list_orders_most_recent_first_and_ages_out() {
        let mut seen = SeenApps::new();
        seen.observe(&[info("old.exe", SessionState::Active, 0.0)], 100);
        seen.observe(&[info("new.exe", SessionState::Active, 0.0)], 900);
        seen.observe(&[info("mid.exe", SessionState::Active, 0.0)], 500);
        let names: Vec<String> = seen
            .list(1000, 1000)
            .into_iter()
            .map(|a| a.exe_name)
            .collect();
        assert_eq!(names, ["new.exe", "mid.exe", "old.exe"]);
        let recent: Vec<String> = seen
            .list(1000, 600)
            .into_iter()
            .map(|a| a.exe_name)
            .collect();
        assert_eq!(recent, ["new.exe", "mid.exe"]);
    }

    #[test]
    fn forget_is_case_insensitive() {
        let mut seen = SeenApps::new();
        seen.observe(&[info("Discord.exe", SessionState::Active, 0.0)], 1);
        seen.forget("DISCORD.EXE");
        assert!(seen.list(1, 10).is_empty());
    }

    #[test]
    fn merge_sessions_combines_same_exe() {
        let a = AudioSessionInfo {
            peak: 0.2,
            ..info("x.exe", SessionState::Inactive, 0.2)
        };
        let b = AudioSessionInfo {
            peak: 0.6,
            ..info("X.EXE", SessionState::Active, 0.6)
        };
        let merged = merge_sessions(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state, SessionState::Active);
        assert!((merged[0].peak - 0.6).abs() < 1e-6);
    }
}
