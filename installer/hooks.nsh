; ClipCheck NSIS installer hooks (Tauri v2 bundler, per-user install).
; Included by the Tauri installer.nsi template via bundle.windows.nsis.installerHooks.
; See installer/README.md for the behaviour and the exit-code contract.
;
; Layout facts this file relies on:
;   - one exe serves as UI and daemon: ClipCheck.exe (daemon mode: --daemon)
;   - install dir: $LOCALAPPDATA\Programs\ClipCheck (currentUser mode)
;   - settings/library: %APPDATA%\ClipCheck; buffer, updates (and the legacy pre-D58 ffmpeg pack folder): %LOCALAPPDATA%\ClipCheck
;   - recordings live in user-chosen folders (default %USERPROFILE%\Videos\ClipCheck) and are NEVER touched
;
; Quoting: command strings use NSIS backtick literals so that apostrophes in $INSTDIR
; (for example C:\Users\O'Brien\...) are safe. Inside those literals a PowerShell "$" is
; written as "$$" and a literal double quote as \" (NSIS does not process backslashes).

!define CC_EXE          "ClipCheck.exe"
!define CC_PROC_NAME    "ClipCheck"
!define CC_RUN_VALUE    "ClipCheck"
!define CC_REG_KEY      "Software\ClipCheck"
!define CC_RUN_KEY      "Software\Microsoft\Windows\CurrentVersion\Run"
; Exit code of `ClipCheck.exe --shutdown-daemon` when the daemon refuses because a
; recording is in progress. Any other code is treated as "could not shut down gracefully".
!define CC_SHUTDOWN_BUSY 3
; Polling for this copy to exit: 20 x 500 ms = 10 s, then force-stop.
!define CC_WAIT_TICKS   20

; Stops ONLY the ClipCheck.exe at $INSTDIR (case-insensitive path match). Other copies
; (portable/dev builds, another install) are never inspected for termination.
;   1. ask this copy's daemon to shut down gracefully (only if the exe exists)
;   2. refuse if it is recording (interactive: message box; silent: abort without UI)
;   3. poll up to 10 s for processes running from $INSTDIR\ClipCheck.exe to exit
;   4. force-stop whatever from that path is still running
!macro CC_StopApp
  ${If} ${FileExists} "$INSTDIR\${CC_EXE}"
    ExecWait `"$INSTDIR\${CC_EXE}" --shutdown-daemon` $0
    ${If} $0 == ${CC_SHUTDOWN_BUSY}
      ${If} ${Silent}
        Abort
      ${Else}
        MessageBox MB_OK|MB_ICONEXCLAMATION "ClipCheck is recording. Stop the recording first, then run this again."
        Abort
      ${EndIf}
    ${EndIf}
  ${EndIf}

  ; Single PowerShell call: returns as soon as no process from this exact path remains,
  ; otherwise stops those processes after ${CC_WAIT_TICKS} x 500 ms.
  ; Nothing to wait for or stop unless this copy is installed. Also keeps a fresh install from
  ; depending on PowerShell starting (nsExec has no timeout).
  ${If} ${FileExists} "$INSTDIR\${CC_EXE}"
    nsExec::ExecToLog `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$p = \"$INSTDIR\${CC_EXE}\"; $$m = { Get-Process -Name ${CC_PROC_NAME} -ErrorAction SilentlyContinue | Where-Object { $$_.Path -and ($$_.Path -ieq $$p) } }; for ($$i = 0; $$i -lt ${CC_WAIT_TICKS}; $$i++) { if (-not (& $$m)) { exit 0 }; Start-Sleep -Milliseconds 500 }; & $$m | Stop-Process -Force -ErrorAction SilentlyContinue; exit 0"`
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CC_StopApp
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "${CC_REG_KEY}" "InstalledVersion" "${VERSION}"
  WriteRegStr HKCU "${CC_REG_KEY}" "InstallDir" "$INSTDIR"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro CC_StopApp
  ; Autostart entry: skip during an updater-driven uninstall (/UPDATE keeps it).
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "${CC_RUN_KEY}" "${CC_RUN_VALUE}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Our own registry keys (the template only clears Software\ClipCheck\ClipCheck, and only
  ; when the data checkbox is ticked).
  DeleteRegKey HKCU "${CC_REG_KEY}"

  ; Settings and library: only when the user ticked "Also remove my ClipCheck settings and
  ; library" (the template's checkbox variable). Never on an updater-driven uninstall.
  ; Only KNOWN app-owned files and folders are removed. No recursive delete is applied to
  ; a parent folder, so a user-chosen buffer or storage folder elsewhere is never touched.
  ; Non-recursive RMDir on a parent succeeds only when it is empty.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; %APPDATA%\ClipCheck: settings and library
    Delete "$APPDATA\ClipCheck\config.toml"
    Delete "$APPDATA\ClipCheck\config.toml.bak"
    Delete "$APPDATA\ClipCheck\library.db*"
    Delete "$APPDATA\ClipCheck\encoder-cache.json"
    Delete "$APPDATA\ClipCheck\update-state.json"
    RMDir /r "$APPDATA\ClipCheck\logs"
    RMDir /r "$APPDATA\ClipCheck\thumbs"
    RMDir "$APPDATA\ClipCheck"

    ; %LOCALAPPDATA%\ClipCheck: buffer, legacy ffmpeg pack folder (pre-D58 installs), updates, logs, sessions
    RMDir /r "$LOCALAPPDATA\ClipCheck\buffer"
    ; Legacy cleanup only: FFmpeg is not shipped or used (D58, D62); keep this for old installs.
    RMDir /r "$LOCALAPPDATA\ClipCheck\ffmpeg"
    RMDir /r "$LOCALAPPDATA\ClipCheck\updates"
    RMDir /r "$LOCALAPPDATA\ClipCheck\logs"
    RMDir /r "$LOCALAPPDATA\ClipCheck\sessions"
    RMDir "$LOCALAPPDATA\ClipCheck"
  ${EndIf}

  ; Always removed: the previous update, and (legacy cleanup only) the FFmpeg pack folder that
  ; versions before D58 downloaded. Nothing creates that folder any more; keep this for old installs.
  RMDir /r "$LOCALAPPDATA\ClipCheck\ffmpeg"
  RMDir /r "$LOCALAPPDATA\ClipCheck\updates\previous"
!macroend
