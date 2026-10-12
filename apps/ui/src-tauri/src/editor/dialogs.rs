//! Native file picker for importing media. Owner: worker `ui-dialogs`.
//!
//! Model: `fileops::show_folder_dialog` (IFileOpenDialog on a blocking thread, COM apartment guard).

/// File-type patterns for `kind == "media"`: video, audio and image types the editor accepts.
const MEDIA_PATTERNS: &str =
    "*.mp4;*.m4v;*.mov;*.m4a;*.mp3;*.wav;*.flac;*.aac;*.ogg;*.wma;*.png;*.jpg;*.jpeg;*.bmp";

/// Shows the native multi-select "Open" dialog. `kind`: "media" (video, audio and image types
/// the editor accepts: mp4 m4v mov m4a mp3 wav flac aac ogg wma png jpg jpeg bmp, shown as one
/// "Media files" filter plus "All files"), or "any". `initial` is a starting folder. Returns the
/// chosen absolute paths (empty vec when the user cancels).
#[tauri::command]
pub async fn editor_pick_files(
    initial: Option<String>,
    kind: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || show_files_dialog(initial, &kind))
        .await
        .map_err(|e| format!("file picker failed: {e}"))?
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

/// Encodes `s` as a NUL-terminated UTF-16 buffer for Win32 `PCWSTR` arguments.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn show_files_dialog(initial: Option<String>, kind: &str) -> Result<Vec<String>, String> {
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, IShellItemArray, SHCreateItemFromParsingName,
        FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH,
    };

    // ERROR_CANCELLED (1223) wrapped as an HRESULT: what Show() returns when the user dismisses the dialog.
    const USER_CANCELLED: HRESULT = HRESULT(0x800704C7_u32 as i32);

    // Filter buffers must outlive the SetFileTypes call, so they are owned here.
    let media_name = wide("Media files");
    let media_spec = wide(MEDIA_PATTERNS);
    let all_name = wide("All files");
    let all_spec = wide("*.*");
    let media_filters = [
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(media_name.as_ptr()),
            pszSpec: PCWSTR(media_spec.as_ptr()),
        },
        COMDLG_FILTERSPEC {
            pszName: PCWSTR(all_name.as_ptr()),
            pszSpec: PCWSTR(all_spec.as_ptr()),
        },
    ];
    let any_filters = [COMDLG_FILTERSPEC {
        pszName: PCWSTR(all_name.as_ptr()),
        pszSpec: PCWSTR(all_spec.as_ptr()),
    }];
    let filters: &[COMDLG_FILTERSPEC] = if kind == "media" {
        &media_filters
    } else {
        &any_filters
    };

    // SAFETY: all COM calls below run on this thread, which is initialized here and uninitialized by
    // the guard. Pointers passed to Win32 are owned or outlive the call (the wide buffers and filter
    // array live to the end of this function).
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if init.is_err() {
            return Err(format!(
                "could not start COM for the file picker ({init:?})"
            ));
        }
        // S_FALSE means COM was already initialized on this thread; it still needs a matching uninit.
        let _apartment = ComApartment(true);

        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("could not create the file picker: {e}"))?;
        let options = dialog
            .GetOptions()
            .map_err(|e| format!("file picker options failed: {e}"))?;
        dialog
            .SetOptions(options | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM)
            .map_err(|e| format!("file picker options failed: {e}"))?;
        dialog
            .SetFileTypes(filters)
            .map_err(|e| format!("file picker filters failed: {e}"))?;

        if let Some(dir) = initial
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            let wide_dir = wide(&dir);
            let start: Result<IShellItem, _> = SHCreateItemFromParsingName(
                PCWSTR(wide_dir.as_ptr()),
                None::<&windows::Win32::System::Com::IBindCtx>,
            );
            if let Ok(item) = start {
                // A bad starting folder is not fatal: the dialog just opens at its default location.
                let _ = dialog.SetFolder(&item);
            }
        }

        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code() == USER_CANCELLED => return Ok(Vec::new()),
            Err(e) => return Err(format!("file picker failed: {e}")),
        }

        let results: IShellItemArray = dialog
            .GetResults()
            .map_err(|e| format!("could not read the selected files: {e}"))?;
        let count = results
            .GetCount()
            .map_err(|e| format!("could not read the selected files: {e}"))?;

        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let item: IShellItem = results
                .GetItemAt(i)
                .map_err(|e| format!("could not read the selected files: {e}"))?;
            let name = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| format!("could not read the selected files: {e}"))?;
            let path = name.to_string();
            CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
            match path {
                Ok(p) => paths.push(p),
                Err(_) => return Err("a selected file has an unreadable name".into()),
            }
        }
        Ok(paths)
    }
}
