//! Process loopback (per-process include/exclude) and default render loopback.

use std::sync::mpsc::Receiver;
use std::sync::OnceLock;

use windows::core::{implement, w, IUnknown, Interface, HRESULT, HSTRING, PCWSTR, PROPVARIANT};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, ActivateAudioInterfaceAsync, EDataFlow,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

use crate::engine::{finish_session, spawn_capture, win_err, CaptureHandle, Session, BUFFER_HNS};
use crate::format::{InputFormat, WAVE_FORMAT_EXTENSIBLE, WAVE_FORMAT_IEEE_FLOAT};
use crate::{AudioChunk, AudioError, LoopbackMode};

/// First Windows 10 build that supports process loopback (2004).
const MIN_PROCESS_LOOPBACK_BUILD: u32 = 19041;
/// How long to wait for the asynchronous activation to complete.
const ACTIVATION_TIMEOUT_MS: u32 = 10_000;
/// E_INVALIDARG.
const E_INVALIDARG: u32 = 0x8007_0057;
/// VT_BLOB variant type tag.
const VT_BLOB: u16 = 65;

/// Mirror of the PROPVARIANT layout for a VT_BLOB value (24 bytes on x64).
/// Only the total size is checked against `windows::core::PROPVARIANT` at compile
/// time (the assert below). The field offsets are not checked.
#[repr(C)]
struct RawBlobVariant {
    vt: u16,
    reserved: [u16; 3],
    cb_size: u32,
    p_blob: *mut u8,
}

const _: () = assert!(std::mem::size_of::<RawBlobVariant>() == std::mem::size_of::<PROPVARIANT>());

/// Activation parameters and the PROPVARIANT that points at them. Heap-allocated
/// so the blob pointer stays valid at a fixed address.
struct ActivationInput {
    raw: RawBlobVariant,
    params: AUDIOCLIENT_ACTIVATION_PARAMS,
}

/// Completion handler for ActivateAudioInterfaceAsync; signals an event.
///
/// Lifetime contract: this object owns the event and the activation input. Both
/// stay alive for as long as COM holds a reference to the handler. The system may
/// still run the activation (and read the blob, and call `SetEvent`) after
/// `activate_audio_client` has timed out and returned, so the event is closed and
/// the input freed only in `Drop`, which runs when the last COM reference goes.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    /// Event handle as an integer (so the struct is `Send`/`Sync`). Closed in `Drop`.
    event: isize,
    /// Kept alive for the asynchronous activation; never read after construction.
    #[allow(dead_code)]
    input: Box<ActivationInput>,
}

impl Drop for ActivationHandler {
    fn drop(&mut self) {
        // SAFETY: `event` was created by CreateEventW and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(HANDLE(self.event as *mut std::ffi::c_void));
        }
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _activateoperation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // SAFETY: `self` holds the event open until this object is destroyed, and
        // this call runs while COM holds a reference to it.
        unsafe { SetEvent(HANDLE(self.event as *mut std::ffi::c_void)) }
    }
}

/// Returns the Windows build number (for example 19045), or `None` if unreadable.
///
/// Read from the registry once per process and cached. The build number of the
/// running OS does not change while the process runs, so later calls skip the
/// registry read entirely. A failed read is cached as `None` too.
pub fn os_build() -> Option<u32> {
    static BUILD: OnceLock<Option<u32>> = OnceLock::new();
    *BUILD.get_or_init(read_os_build)
}

/// Reads `CurrentBuildNumber` from the registry. Called once, via [`os_build`].
fn read_os_build() -> Option<u32> {
    let mut buf = [0u16; 32];
    let mut size = (buf.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: buf and size are valid for the duration of the call; the value is REG_SZ.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    if status.0 != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().parse().ok()
}

/// Checks that process loopback is available (Windows 10 build 19041 or newer).
pub fn is_supported() -> Result<(), AudioError> {
    match os_build() {
        Some(b) if b >= MIN_PROCESS_LOOPBACK_BUILD => Ok(()),
        _ => Err(AudioError::Unsupported(
            "process loopback requires Windows 10 build 19041 or newer",
        )),
    }
}

/// Activates an `IAudioClient` through `ActivateAudioInterfaceAsync` using the
/// given activation parameters. Blocks until the activation completes.
fn activate_audio_client(
    params: AUDIOCLIENT_ACTIVATION_PARAMS,
) -> Result<IAudioClient, AudioError> {
    let mut input = Box::new(ActivationInput {
        raw: RawBlobVariant {
            vt: VT_BLOB,
            reserved: [0; 3],
            cb_size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            p_blob: std::ptr::null_mut(),
        },
        params,
    });
    // The blob points into the same heap allocation as the variant, so both move together
    // into the completion handler and stay at a fixed address.
    input.raw.p_blob = &mut input.params as *mut AUDIOCLIENT_ACTIVATION_PARAMS as *mut u8;

    // SAFETY: the event is created here and then owned by the completion handler, which
    // closes it on drop. `input` is owned by the same handler, so the blob outlives the
    // activation even if this function returns early on timeout.
    unsafe {
        let event =
            CreateEventW(None, false, false, PCWSTR::null()).map_err(win_err("CreateEventW"))?;
        activate_with_event(event, input)
    }
}

/// Body of [`activate_audio_client`] once the event exists. Takes ownership of
/// `event` and `input`; they are released when the completion handler is destroyed.
///
/// # Safety
/// `event` must be a valid, auto-reset event handle that nothing else closes.
unsafe fn activate_with_event(
    event: HANDLE,
    input: Box<ActivationInput>,
) -> Result<IAudioClient, AudioError> {
    let pv = &input.raw as *const RawBlobVariant as *const PROPVARIANT;
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        event: event.0 as isize,
        input,
    }
    .into();
    let op = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(pv),
        &handler,
    )
    .map_err(win_err("ActivateAudioInterfaceAsync"))?;

    // `handler` is alive here, so the event is still open. On timeout we return, but the
    // handler (and therefore the event and input) lives on until COM releases it.
    if WaitForSingleObject(event, ACTIVATION_TIMEOUT_MS) != WAIT_OBJECT_0 {
        return Err(AudioError::Unsupported(
            "process loopback activation timed out",
        ));
    }

    let mut hr = HRESULT(0);
    let mut iface: Option<IUnknown> = None;
    op.GetActivateResult(&mut hr, &mut iface).map_err(win_err(
        "IActivateAudioInterfaceAsyncOperation::GetActivateResult",
    ))?;
    if hr.is_err() {
        return Err(AudioError::Win {
            context: "ActivateAudioInterfaceAsync",
            hresult: hr.0 as u32,
        });
    }
    let unknown = iface.ok_or(AudioError::NoDevice)?;
    unknown
        .cast::<IAudioClient>()
        .map_err(win_err("IUnknown::cast<IAudioClient>"))
}

/// Opens a process loopback session for `pid` on the calling (capture) thread.
fn open_process_loopback(pid: u32, mode: LoopbackMode) -> Result<Session, AudioError> {
    let mode = match mode {
        LoopbackMode::IncludeTree => PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        LoopbackMode::ExcludeTree => PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    };
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: mode,
            },
        },
    };
    let client = activate_audio_client(params)?;

    // The process loopback client does not implement GetMixFormat, so the
    // format is fixed: IEEE float, 2 channels, 48 kHz, 32 bit.
    let wfx = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: 2,
        nSamplesPerSec: 48_000,
        nAvgBytesPerSec: 384_000,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // SAFETY: client is a freshly activated, uninitialised IAudioClient; wfx lives across the call.
    unsafe {
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, &wfx, None)
            .map_err(win_err("IAudioClient::Initialize (process loopback)"))?;
    }
    finish_session(
        client,
        InputFormat {
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 32,
            is_float: true,
        },
    )
}

/// Reads a WAVEFORMATEX (plus extension, if present) into an [`InputFormat`].
///
/// The only work done here is the unsafe read of the SubFormat tag. The tag
/// mapping and validation are in [`InputFormat::from_wave_fields`], which is
/// the single parse path.
///
/// # Safety
/// `p` must point to a WAVEFORMATEX returned by `GetMixFormat`, including its
/// `cbSize` extension bytes.
unsafe fn read_mix_format(p: *const WAVEFORMATEX) -> Result<InputFormat, AudioError> {
    let wf = std::ptr::read_unaligned(p);
    let sub_tag = if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22 {
        // SubFormat GUID starts at byte 24 of WAVEFORMATEXTENSIBLE; its Data1 holds the tag.
        let data1 = std::ptr::read_unaligned((p as *const u8).add(24) as *const u32);
        (data1 & 0xFFFF) as u16
    } else {
        0
    };
    InputFormat::from_wave_fields(
        wf.wFormatTag,
        sub_tag,
        wf.nChannels,
        wf.nSamplesPerSec,
        wf.wBitsPerSample,
    )
}

/// Opens an MMDevice endpoint (default when `id` is None) in shared mode.
/// With `loopback`, a render endpoint is captured as a loopback of its mix.
pub(crate) fn open_mmdevice(
    flow: EDataFlow,
    id: Option<&str>,
    loopback: bool,
) -> Result<Session, AudioError> {
    // SAFETY: called on the capture thread after COM has been initialised as MTA.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(win_err("CoCreateInstance(MMDeviceEnumerator)"))?;
        // A malformed explicit ID (E_INVALIDARG) names no endpoint, so report it as NoDevice.
        let device = match id {
            Some(id) => enumerator.GetDevice(&HSTRING::from(id)),
            None => enumerator.GetDefaultAudioEndpoint(flow, eConsole),
        }
        .map_err(|e| {
            if e.code().0 as u32 == E_INVALIDARG {
                AudioError::NoDevice
            } else {
                win_err("IMMDeviceEnumerator endpoint lookup")(e)
            }
        })?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(win_err("IMMDevice::Activate"))?;

        let mix = client
            .GetMixFormat()
            .map_err(win_err("IAudioClient::GetMixFormat"))?;
        let format = read_mix_format(mix);
        let init = match &format {
            Ok(_) => {
                let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | if loopback {
                        AUDCLNT_STREAMFLAGS_LOOPBACK
                    } else {
                        0
                    };
                client
                    .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, mix, None)
                    .map_err(win_err("IAudioClient::Initialize"))
            }
            Err(e) => Err(e.clone()),
        };
        CoTaskMemFree(Some(mix as *const std::ffi::c_void));
        init?;
        finish_session(client, format?)
    }
}

/// Per-process audio capture (include or exclude a process tree).
pub struct ProcessLoopback {
    handle: CaptureHandle,
}

impl ProcessLoopback {
    /// Starts capturing audio for `pid` according to `mode`.
    ///
    /// Returns `AudioError::Unsupported` on Windows builds older than 19041.
    pub fn start(pid: u32, mode: LoopbackMode) -> Result<(Self, Receiver<AudioChunk>), AudioError> {
        is_supported()?;
        let (handle, rx) = spawn_capture("clipcheck-proc-loopback", move || {
            open_process_loopback(pid, mode)
        })?;
        Ok((Self { handle }, rx))
    }

    /// Stops capture and joins the capture thread.
    pub fn stop(&mut self) {
        self.handle.stop();
    }

    /// The error that ended capture, if the capture thread stopped because of a
    /// device or conversion failure. `None` while running or after a normal stop.
    pub fn last_error(&self) -> Option<AudioError> {
        self.handle.error()
    }
}

/// Whole-system mix of the default render endpoint (device loopback).
pub struct SystemMix {
    handle: CaptureHandle,
}

impl SystemMix {
    /// Starts loopback capture of the default render endpoint.
    pub fn start() -> Result<(Self, Receiver<AudioChunk>), AudioError> {
        let (handle, rx) = spawn_capture("clipcheck-system-mix", || {
            open_mmdevice(eRender, None, true)
        })?;
        Ok((Self { handle }, rx))
    }

    /// Stops capture and joins the capture thread.
    pub fn stop(&mut self) {
        self.handle.stop();
    }

    /// The error that ended capture, if the capture thread stopped because of a
    /// device or conversion failure. `None` while running or after a normal stop.
    pub fn last_error(&self) -> Option<AudioError> {
        self.handle.error()
    }
}
