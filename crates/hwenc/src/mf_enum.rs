//! MFStartup and encoder enumeration (worker H1).

use std::sync::OnceLock;

use windows::core::{GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFStartup, MFTEnumEx, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
    MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO, MF_VERSION,
};
use windows::Win32::System::Com::{CoIncrementMTAUsage, CoTaskMemFree};

use crate::{Codec, EncError, EncoderDescriptor, Vendor};

/// Cached result of the process-wide `MFStartup` call.
static STARTUP: OnceLock<Result<(), EncError>> = OnceLock::new();

/// MFStartup once per process.
///
/// Takes a process-wide MTA reference first (`CoIncrementMTAUsage`, cookie intentionally kept
/// alive for the whole process) so Media Foundation has COM available on any calling thread.
/// The result, success or failure, is cached and returned on every later call.
pub(crate) fn startup() -> Result<(), EncError> {
    STARTUP.get_or_init(do_startup).clone()
}

/// Performs the actual COM and Media Foundation startup. Called at most once per process.
fn do_startup() -> Result<(), EncError> {
    // SAFETY: CoIncrementMTAUsage takes no pointers from us beyond the out-cookie it writes
    // itself; calling it has no preconditions besides COM being loadable (ole32.dll).
    // The cookie is dropped on purpose: the MTA reference must live for the whole process.
    let _cookie = unsafe { CoIncrementMTAUsage() }.map_err(EncError::win("CoIncrementMTAUsage"))?;
    // SAFETY: MFStartup has no pointer arguments; MF_VERSION and MFSTARTUP_FULL are valid
    // constants from the SDK. It may be called from any thread once COM is available.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(EncError::win("MFStartup"))
}

/// Encoders for `codec`, hardware first.
///
/// Returns an empty list when the machine has no matching encoder.
pub(crate) fn enumerate(codec: Codec) -> Result<Vec<EncoderDescriptor>, EncError> {
    Ok(list_activates(codec)?
        .into_iter()
        .map(|(desc, _)| desc)
        .collect())
}

/// The activation object for `desc` (re-enumerates and picks `desc.index`, checking the name matches).
#[allow(dead_code)] // used by mf_async / mf_sync once implemented (H2/H3)
pub(crate) fn activate_for(desc: &EncoderDescriptor) -> Result<IMFActivate, EncError> {
    let (found, activate) = list_activates(desc.codec)?
        .into_iter()
        .nth(desc.index)
        .ok_or_else(|| EncError::NotFound(desc.name.clone()))?;
    if found.name != desc.name {
        return Err(EncError::NotFound(desc.name.clone()));
    }
    Ok(activate)
}

/// Enumerates the raw activation objects for `codec`, hardware first, then software.
///
/// Each descriptor's `index` is its position in the returned vector.
fn list_activates(codec: Codec) -> Result<Vec<(EncoderDescriptor, IMFActivate)>, EncError> {
    startup()?;

    let output_subtype = match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
    };
    let input_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: output_subtype,
    };

    let hardware_flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let software_flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);

    let hardware = enum_raw(hardware_flags, &input_type, &output_type)?;
    let software = enum_raw(software_flags, &input_type, &output_type)?;

    let mut out = Vec::with_capacity(hardware.len() + software.len());
    for (is_hardware, activates) in [(true, hardware), (false, software)] {
        for activate in activates {
            let index = out.len();
            let (name, vendor) = describe(&activate, is_hardware);
            let desc = EncoderDescriptor {
                name,
                vendor,
                codec,
                hardware: is_hardware,
                index,
            };
            out.push((desc, activate));
        }
    }
    Ok(out)
}

/// One `MFTEnumEx` call. Takes ownership of every returned activation object and frees the array.
fn enum_raw(
    flags: MFT_ENUM_FLAG,
    input_type: &MFT_REGISTER_TYPE_INFO,
    output_type: &MFT_REGISTER_TYPE_INFO,
) -> Result<Vec<IMFActivate>, EncError> {
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;

    // SAFETY: `input_type` and `output_type` are valid for the duration of the call and
    // MFTEnumEx only reads them. `array` and `count` are valid out-pointers. On success MF
    // writes a CoTaskMem-allocated array of `count` Option<IMFActivate> into `array`.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(input_type as *const MFT_REGISTER_TYPE_INFO),
            Some(output_type as *const MFT_REGISTER_TYPE_INFO),
            &mut array,
            &mut count,
        )
    }
    .map_err(EncError::win("MFTEnumEx"))?;

    if array.is_null() || count == 0 {
        if !array.is_null() {
            // SAFETY: a non-null array was allocated by MF with CoTaskMemAlloc; free it.
            unsafe { CoTaskMemFree(Some(array as *const core::ffi::c_void)) };
        }
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: MF guarantees `count` elements at `array`; each is an Option<IMFActivate>
        // (pointer-sized). Reading moves the element out, so ownership passes to us and the
        // array itself is freed below without dropping the elements a second time.
        let item = unsafe { array.add(i).read() };
        if let Some(activate) = item {
            out.push(activate);
        }
    }

    // SAFETY: the array pointer came from MFTEnumEx and is no longer read after this point.
    unsafe { CoTaskMemFree(Some(array as *const core::ffi::c_void)) };
    Ok(out)
}

/// Reads the friendly name and vendor of one activation object.
fn describe(activate: &IMFActivate, hardware: bool) -> (String, Vendor) {
    let name = get_allocated_string(activate, &MFT_FRIENDLY_NAME_Attribute)
        .unwrap_or_else(|| "Unknown encoder".to_string());

    let vendor = if hardware {
        match get_allocated_string(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute) {
            Some(id) => vendor_from_id(&id),
            None => Vendor::Other,
        }
    } else {
        // Software encoder MFTs registered on Windows are Microsoft's ("H264 Encoder MFT").
        Vendor::Microsoft
    };
    (name, vendor)
}

/// Maps a PCI vendor string such as `VEN_10DE` to a [`Vendor`].
fn vendor_from_id(id: &str) -> Vendor {
    let upper = id.to_ascii_uppercase();
    if upper.contains("VEN_10DE") {
        Vendor::Nvidia
    } else if upper.contains("VEN_1002") {
        Vendor::Amd
    } else if upper.contains("VEN_8086") {
        Vendor::Intel
    } else {
        Vendor::Other
    }
}

/// Reads a string attribute of `activate`, frees the Windows buffer, and returns `None` when the
/// attribute is missing or not valid UTF-16.
fn get_allocated_string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut value = PWSTR::null();
    let mut length: u32 = 0;
    // SAFETY: `key` is a valid GUID reference for the call. On success MF writes a
    // CoTaskMem-allocated wide string into `value` and its length into `length`.
    let result = unsafe { activate.GetAllocatedString(key, &mut value, &mut length) };
    if result.is_err() || value.is_null() {
        return None;
    }
    // SAFETY: `value` is a valid, NUL-terminated wide string allocated by MF and is read
    // only once here, before it is freed below.
    let text = unsafe { value.to_string() }.ok();
    // SAFETY: `value` was allocated by GetAllocatedString with CoTaskMemAlloc and is not used
    // after this call.
    unsafe { CoTaskMemFree(Some(value.0 as *const core::ffi::c_void)) };
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_enumerate_h264() {
        let list = enumerate(Codec::H264).expect("enumerate H.264 encoders");
        eprintln!("H.264 encoders on this machine ({}):", list.len());
        for d in &list {
            eprintln!(
                "  [{}] {} | {:?} | hardware={}",
                d.index, d.name, d.vendor, d.hardware
            );
        }
    }
}
