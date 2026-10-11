//! Audio endpoint (device) discovery: which capture and render devices are
//! active, their friendly names, and which one is the console default.

use crate::AudioError;

/// Direction of an audio endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointKind {
    /// A capture endpoint (microphone, line-in).
    Capture,
    /// A render endpoint (speakers, headphones).
    Render,
}

/// An active audio endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Endpoint ID, usable with `Microphone::start` and other by-ID lookups.
    pub id: String,
    /// Friendly name (falls back to the ID when the name is unavailable).
    pub name: String,
    /// True for the console default endpoint of its kind.
    pub is_default: bool,
    /// Direction of the endpoint.
    pub kind: EndpointKind,
}

/// Sorts endpoints for display: the default first, then by name
/// (case-insensitive), then by ID.
pub fn sort_endpoints(endpoints: &mut [Endpoint]) {
    endpoints.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Lists the active endpoints of `kind`, in display order (see [`sort_endpoints`]).
///
/// An empty list is a valid result (for example, no microphone is attached).
#[cfg(windows)]
pub fn list_endpoints(kind: EndpointKind) -> Result<Vec<Endpoint>, AudioError> {
    let _com = win::com_scope();
    let devices = win::active_devices(kind)?;
    let default_id = win::default_device_id(kind);
    let mut out = Vec::with_capacity(devices.len());
    for device in &devices {
        let Some(id) = win::device_id(device) else {
            continue;
        };
        let name = win::device_friendly_name(device).unwrap_or_else(|| id.clone());
        let is_default = default_id.as_deref() == Some(id.as_str());
        out.push(Endpoint {
            id,
            name,
            is_default,
            kind,
        });
    }
    sort_endpoints(&mut out);
    Ok(out)
}

/// Non-Windows stub: endpoint discovery needs WASAPI.
#[cfg(not(windows))]
pub fn list_endpoints(_kind: EndpointKind) -> Result<Vec<Endpoint>, AudioError> {
    Err(AudioError::Unsupported("audio endpoints require Windows"))
}

/// WASAPI helpers shared with the session and meter modules.
#[cfg(windows)]
pub(crate) mod win {
    use std::ffi::c_void;

    use windows::core::PWSTR;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, EDataFlow, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    use super::EndpointKind;
    use crate::engine::win_err;
    use crate::AudioError;

    /// Keeps a multithreaded apartment alive for the whole process (see DESIGN.md D20).
    ///
    /// The `windows` crate caches activation factories process-wide; if the only MTA
    /// holder exits, those factories dangle. Called once from every entry point.
    pub(crate) fn ensure_process_mta() {
        static MTA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        MTA.get_or_init(|| {
            // SAFETY: no preconditions; the cookie is intentionally never released so the
            // MTA stays alive until process exit.
            let _ = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
        });
    }

    /// Per-call COM initialisation on the calling thread, balanced on drop when it succeeded.
    pub(crate) struct ComScope {
        owned: bool,
    }

    /// Initialises COM as MTA on the current thread for the lifetime of the returned scope.
    pub(crate) fn com_scope() -> ComScope {
        ensure_process_mta();
        // SAFETY: CoInitializeEx has no memory preconditions. S_FALSE (already MTA) is ok and
        // balanced; RPC_E_CHANGED_MODE (already STA) is not balanced.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComScope { owned: hr.is_ok() }
    }

    impl Drop for ComScope {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: matches the successful CoInitializeEx in `com_scope`.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn flow(kind: EndpointKind) -> EDataFlow {
        match kind {
            EndpointKind::Capture => eCapture,
            EndpointKind::Render => eRender,
        }
    }

    fn enumerator() -> Result<IMMDeviceEnumerator, AudioError> {
        // SAFETY: called on a thread with COM initialised (see `com_scope`).
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(win_err("CoCreateInstance(MMDeviceEnumerator)"))
    }

    /// All active endpoints of `kind`, in the system's enumeration order.
    pub(crate) fn active_devices(kind: EndpointKind) -> Result<Vec<IMMDevice>, AudioError> {
        let e = enumerator()?;
        // SAFETY: the enumerator is valid for the duration of these calls.
        unsafe {
            let coll = e
                .EnumAudioEndpoints(flow(kind), DEVICE_STATE_ACTIVE)
                .map_err(win_err("IMMDeviceEnumerator::EnumAudioEndpoints"))?;
            let n = coll
                .GetCount()
                .map_err(win_err("IMMDeviceCollection::GetCount"))?;
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                if let Ok(d) = coll.Item(i) {
                    out.push(d);
                }
            }
            Ok(out)
        }
    }

    /// ID of the console default endpoint of `kind`, if one exists.
    pub(crate) fn default_device_id(kind: EndpointKind) -> Option<String> {
        let e = enumerator().ok()?;
        // SAFETY: the enumerator is valid for the duration of the call.
        let d = unsafe { e.GetDefaultAudioEndpoint(flow(kind), eConsole) }.ok()?;
        device_id(&d)
    }

    /// Endpoint ID string of a device.
    pub(crate) fn device_id(d: &IMMDevice) -> Option<String> {
        // SAFETY: the device is a valid COM object owned by the caller.
        let p = unsafe { d.GetId() }.ok()?;
        Some(take_co_string(p))
    }

    /// `PKEY_Device_FriendlyName` of a device, when present and non-empty.
    pub(crate) fn device_friendly_name(d: &IMMDevice) -> Option<String> {
        // SAFETY: the device is a valid COM object; the property store and value are owned locally.
        unsafe {
            let store: IPropertyStore = d.OpenPropertyStore(STGM_READ).ok()?;
            let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let s = windows::core::BSTR::try_from(&value).ok()?.to_string();
            (!s.is_empty()).then_some(s)
        }
    }

    /// Copies a COM-allocated wide string into a `String` and frees the original.
    pub(crate) fn take_co_string(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: `p` is a NUL-terminated string allocated by COM and freed exactly once below.
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const c_void));
            s
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn ep(id: &str, name: &str, is_default: bool) -> Endpoint {
        Endpoint {
            id: id.into(),
            name: name.into(),
            is_default,
            kind: EndpointKind::Render,
        }
    }

    #[test]
    fn default_first_then_name() {
        let mut v = vec![
            ep("a", "Zeta", false),
            ep("b", "alpha", false),
            ep("c", "Beta", true),
        ];
        sort_endpoints(&mut v);
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
    }
}
