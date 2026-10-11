//! File-system helpers the UI calls: reveal a recording in Explorer, open a folder, pick a folder.

use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Expands `%NAME%` references using the process environment. Unknown names and unmatched `%`
/// are left exactly as written.
fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Opens Explorer with the recording's file selected.
#[tauri::command]
pub fn reveal_recording(id: String) -> Result<(), String> {
    let db = crate::commands::try_default_db_path().map_err(|e| e.to_string())?;
    let path = crate::media::resolve_recording_path(&db, &id)
        .map_err(|e| format!("could not find the recording: {e}"))?;
    if !path.is_file() {
        return Err(format!("recording file is missing: {}", path.display()));
    }
    // Explorer wants /select,"<path>" with only the path quoted. A normal arg() would quote the
    // whole /select,... token when the path has spaces, and Explorer then opens Documents instead.
    let mut cmd = Command::new("explorer.exe");
    #[cfg(windows)]
    cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    #[cfg(not(windows))]
    cmd.arg(format!("/select,{}", path.display()));
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open Explorer: {e}"))
}

/// Opens `path` (environment variables such as %USERPROFILE% are expanded) in Explorer.
/// The folder is created first when it does not exist.
#[tauri::command]
pub fn open_folder(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("no folder given".into());
    }
    let expanded = expand_env(trimmed);
    let folder = Path::new(&expanded);
    if !folder.exists() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("could not create folder {}: {e}", folder.display()))?;
    }
    Command::new("explorer.exe")
        .arg(folder)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open Explorer: {e}"))
}

/// Shows the native folder picker. `None` when the user cancels.
/// Runs on a blocking thread so the UI thread stays responsive while the dialog is open.
#[tauri::command]
pub async fn pick_folder(initial: Option<String>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || show_folder_dialog(initial))
        .await
        .map_err(|e| format!("folder picker failed: {e}"))?
}

/// Balances a successful `CoInitializeEx` with `CoUninitialize` when dropped.
struct ComApartment(bool);

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with a successful CoInitializeEx on this same thread.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn show_folder_dialog(initial: Option<String>) -> Result<Option<String>, String> {
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    // ERROR_CANCELLED (1223) wrapped as an HRESULT: what Show() returns when the user dismisses the dialog.
    const USER_CANCELLED: HRESULT = HRESULT(0x800704C7_u32 as i32);

    // SAFETY: all COM calls below run on this thread, which is initialized here and uninitialized by
    // the guard. Pointers passed to Win32 are owned or outlive the call (the wide buffer lives to the end).
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if init.is_err() {
            return Err(format!(
                "could not start COM for the folder picker ({init:?})"
            ));
        }
        // S_FALSE means COM was already initialized on this thread; it still needs a matching uninit.
        let _apartment = ComApartment(true);

        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("could not create the folder picker: {e}"))?;
        let options = dialog
            .GetOptions()
            .map_err(|e| format!("folder picker options failed: {e}"))?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .map_err(|e| format!("folder picker options failed: {e}"))?;

        if let Some(dir) = initial
            .map(|s| expand_env(s.trim()))
            .filter(|s| !s.is_empty())
        {
            let wide: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
            let start: Result<IShellItem, _> = SHCreateItemFromParsingName(
                PCWSTR(wide.as_ptr()),
                None::<&windows::Win32::System::Com::IBindCtx>,
            );
            if let Ok(item) = start {
                // A bad starting folder is not fatal: the dialog just opens at its default location.
                let _ = dialog.SetFolder(&item);
            }
        }

        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code() == USER_CANCELLED => return Ok(None),
            Err(e) => return Err(format!("folder picker failed: {e}")),
        }

        let item = dialog
            .GetResult()
            .map_err(|e| format!("could not read the selected folder: {e}"))?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| format!("could not read the selected folder: {e}"))?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
        match path {
            Ok(p) => Ok(Some(p)),
            Err(_) => Err("the selected folder has an unreadable name".into()),
        }
    }
}
