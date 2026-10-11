//! The tray icon owned by the recorder daemon.
//!
//! The daemon owns the icon so it stays alive when the UI window closes.
//! [`Tray::spawn`] starts a thread with a hidden top-level window (top-level
//! so it receives the `TaskbarCreated` broadcast), adds the icon with
//! `Shell_NotifyIconW`, and runs the window's message loop.
//!
//! - Left click or double click opens the UI ([`TrayHandlers::open_ui`]).
//! - Right click shows a menu with exactly two items, "Open ClipCheck" and "Exit".
//! - "Exit" goes through [`decide_exit`]: while a session records and
//!   `general.confirm_exit_while_recording` is set, the user is asked first.
//!
//! The plain functions here ([`menu_items`], [`decide_exit`], [`tooltip_text`],
//! [`ui_target`]) hold the decisions and compile on every platform. Only the Win32
//! window code is `cfg(windows)`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use clipcheck_ipc::proto::StatusDto;
use clipcheck_util::sync::lock_unpoisoned as lock;
use serde_json::Value;

use crate::logging;
use crate::service::Service;

/// Callback message the shell sends to the tray window (`WM_APP + 1`).
pub const CALLBACK_MESSAGE: u32 = 0x8001;
/// Internal message: the requested tooltip changed (`WM_APP + 2`).
pub(crate) const TOOLTIP_MESSAGE: u32 = 0x8002;
/// Window class name of the hidden tray window.
pub const TRAY_CLASS: &str = "ClipCheckTray";
/// Menu id of "Open ClipCheck" (the default item).
pub const MENU_ID_OPEN: u16 = 1;
/// Menu id of "Exit".
pub const MENU_ID_EXIT: u16 = 2;
/// Tooltip text when nothing is recording.
pub const APP_TOOLTIP: &str = "ClipCheck";
/// Fewest time between two tooltip updates.
pub const TOOLTIP_MIN_INTERVAL: Duration = Duration::from_secs(5);
/// Question shown before exiting while a session records.
pub const CONFIRM_EXIT_TEXT: &str =
    "A recording session is in progress. Exit ClipCheck and stop recording?";

/// Delay between attempts to add the icon while the shell is not ready.
pub(crate) const ADD_RETRY_DELAY_MS: u32 = 2_000;
/// Retries after the first failed add. 15 retries at 2 s cover about 30 s.
pub(crate) const ADD_RETRY_LIMIT: u32 = 15;
/// Timer id for the add retry.
pub(crate) const TIMER_ADD_RETRY: usize = 1;
/// Timer id for the deferred tooltip update.
pub(crate) const TIMER_TOOLTIP: usize = 2;
/// Notify-icon id of our one icon.
pub(crate) const TRAY_UID: u32 = 1;

/// Set while the exit confirm dialog is open. The tray thread then sits in a
/// modal loop, so shutdown must not wait for it.
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);

/// The two actions the tray can ask the daemon to take.
pub struct TrayHandlers {
    /// Opens the UI (left click, double click, or "Open ClipCheck").
    pub open_ui: Box<dyn Fn() + Send + Sync>,
    /// Asks to exit ("Exit"). The handler decides whether to exit.
    pub request_exit: Box<dyn Fn() + Send + Sync>,
}

/// One item of the tray menu. The enum is the only place a menu item's id and
/// label are defined, so `match` arms below cover every item at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    /// "Open ClipCheck" (the default item).
    Open,
    /// "Exit". Goes through the exit confirmation.
    Exit,
}

impl MenuItem {
    /// The menu, in display order.
    pub const ALL: [MenuItem; 2] = [MenuItem::Open, MenuItem::Exit];

    /// Command id sent with `WM_COMMAND` when the item is chosen.
    pub fn id(self) -> u16 {
        match self {
            MenuItem::Open => MENU_ID_OPEN,
            MenuItem::Exit => MENU_ID_EXIT,
        }
    }

    /// Text shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Open => "Open ClipCheck",
            MenuItem::Exit => "Exit",
        }
    }

    /// The item for a command id, or `None` when the id is not a menu item.
    pub fn from_id(id: u16) -> Option<MenuItem> {
        MenuItem::ALL.into_iter().find(|item| item.id() == id)
    }
}

/// The menu, in display order, as `(item, label)` pairs. Exactly two items.
pub fn menu_items() -> [(MenuItem, &'static str); 2] {
    MenuItem::ALL.map(|item| (item, item.label()))
}

/// Decides whether the daemon may exit now.
///
/// Asks the user only when a session is active AND `confirm_enabled` is set.
/// `user_confirms` is not called otherwise. Returns true when the exit may go on.
pub fn decide_exit(
    session_active: bool,
    confirm_enabled: bool,
    user_confirms: impl FnOnce() -> bool,
) -> bool {
    if session_active && confirm_enabled {
        user_confirms()
    } else {
        true
    }
}

/// Asks the service whether to exit and applies [`decide_exit`].
///
/// A service that cannot answer the status query is treated as recording, so the
/// user still gets the question. A config that cannot be read defaults the check on.
pub fn exit_confirmed(service: &Service, confirm: impl FnOnce() -> bool) -> bool {
    let session_active = match service.status() {
        Ok(status) => status.session.is_some() || test_session_override(),
        Err(e) => {
            logging::warn(&format!("exit check: status unavailable ({})", e.message));
            true
        }
    };
    let confirm_enabled = match service.config() {
        Ok(view) => view
            .config
            .pointer("/general/confirm_exit_while_recording")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        Err(_) => true,
    };
    decide_exit(session_active, confirm_enabled, confirm)
}

/// Formats seconds as `H:MM:SS`, with hours unbounded.
pub fn format_elapsed(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Tooltip text. `recording` is `None` when idle; otherwise the game name (if any)
/// and the elapsed session time in seconds.
pub fn tooltip_text(recording: Option<(Option<&str>, u64)>) -> String {
    match recording {
        None => APP_TOOLTIP.to_owned(),
        Some((Some(game), secs)) if !game.is_empty() => {
            format!(
                "ClipCheck \u{2014} recording {game} {}",
                format_elapsed(secs)
            )
        }
        Some((_, secs)) => format!("ClipCheck \u{2014} recording {}", format_elapsed(secs)),
    }
}

/// Tooltip text for a status snapshot. `now_ms` is the current Unix time in ms.
pub fn tooltip_for_status(status: &StatusDto, now_ms: i64) -> String {
    let recording = status.session.as_ref().map(|session| {
        let elapsed_ms = now_ms.saturating_sub(session.started_unix_ms).max(0);
        let secs = u64::try_from(elapsed_ms / 1000).unwrap_or(0);
        let game = status.game_info.as_ref().map(|g| g.name.as_str());
        (game, secs)
    });
    tooltip_text(recording)
}

/// Picks the executable the "Open ClipCheck" action starts.
///
/// From `clipcheckd.exe` it is the sibling `ClipCheck.exe` when `sibling_ui_exists`.
/// Otherwise it is `current_exe` itself, started without `--daemon`.
pub fn ui_target(current_exe: &Path, sibling_ui_exists: bool) -> PathBuf {
    let is_daemon_exe = current_exe
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("clipcheckd.exe"));
    if is_daemon_exe && sibling_ui_exists {
        current_exe.with_file_name("ClipCheck.exe")
    } else {
        current_exe.to_path_buf()
    }
}

/// Starts the UI. Used by the tray's "Open" action.
///
/// The UI has a single-instance guard, so a second launch only focuses the
/// running window. The child is detached: the daemon never waits for it.
/// In debug builds, `CLIPCHECK_UI_COMMAND_OVERRIDE` names a program to run
/// instead of the UI (used by the tests).
pub fn launch_ui() {
    // One click reaches the tray as WM_LBUTTONUP and NIN_SELECT (and a double click adds
    // WM_LBUTTONDBLCLK). Those would spawn several UI processes that race for the UI's
    // single-instance lock, so repeated requests inside a short window collapse into one.
    if !open_request_allowed() {
        return;
    }
    if let Some(program) = ui_command_override() {
        spawn_detached(&program);
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            logging::warn(&format!("cannot find the daemon exe to open the UI: {e}"));
            return;
        }
    };
    let sibling_exists = exe.with_file_name("ClipCheck.exe").exists();
    spawn_detached(&ui_target(&exe, sibling_exists));
}

/// Minimum gap between two UI launches from the tray.
const OPEN_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(1500);

/// True when this open request should run; false when it repeats one made just before.
fn open_request_allowed() -> bool {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if last.is_some_and(|t| now.duration_since(t) < OPEN_DEBOUNCE) {
        return false;
    }
    *last = Some(now);
    true
}

/// Asks the user whether to exit while recording. The default answer is No.
///
/// Debug builds honor `CLIPCHECK_TEST_CONFIRM=yes|no` and skip the dialog.
pub fn confirm_exit() -> bool {
    if let Some(answer) = test_confirm_override() {
        return answer;
    }
    confirm_dialog()
}

fn spawn_detached(program: &Path) {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: the UI is a console app in debug builds; no console may appear.
        clipcheck_util::proc::no_window(&mut cmd);
    }
    match cmd.spawn() {
        Ok(child) => {
            logging::info(&format!("opened {}", program.display()));
            // Dropping the child handle leaves the process running on its own.
            drop(child);
        }
        Err(e) => logging::warn(&format!("cannot start {}: {e}", program.display())),
    }
}

#[cfg(debug_assertions)]
fn ui_command_override() -> Option<PathBuf> {
    std::env::var_os("CLIPCHECK_UI_COMMAND_OVERRIDE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(debug_assertions))]
fn ui_command_override() -> Option<PathBuf> {
    None
}

/// Debug builds only: `CLIPCHECK_TEST_SESSION_ACTIVE=1` makes the exit check treat
/// a session as active, so the confirm path can be tested without an encoder or a game.
#[cfg(debug_assertions)]
fn test_session_override() -> bool {
    std::env::var("CLIPCHECK_TEST_SESSION_ACTIVE").is_ok_and(|v| v == "1")
}

#[cfg(not(debug_assertions))]
fn test_session_override() -> bool {
    false
}

#[cfg(debug_assertions)]
fn test_confirm_override() -> Option<bool> {
    match std::env::var("CLIPCHECK_TEST_CONFIRM").ok()?.as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

#[cfg(not(debug_assertions))]
fn test_confirm_override() -> Option<bool> {
    None
}

#[cfg(windows)]
fn confirm_dialog() -> bool {
    win::confirm_dialog()
}

#[cfg(not(windows))]
fn confirm_dialog() -> bool {
    // No native dialog off Windows; exiting is the only meaningful answer there.
    true
}

/// Tooltip text shared between callers and the tray thread.
#[derive(Default)]
struct TooltipSlot {
    /// Text the tray thread should apply next.
    pending: Mutex<Option<String>>,
    /// Text last requested. Used to skip repeats.
    last_requested: Mutex<String>,
}

/// Owner of the tray thread. Use [`Tray::spawn`].
pub struct Tray;

impl Tray {
    /// Starts the tray thread and adds the icon.
    ///
    /// Never fails. If the window or icon cannot be created the daemon keeps
    /// running without a tray; the reason is logged. A failed icon add is retried
    /// every 2 s for about 30 s, which covers a shell that is not up yet.
    pub fn spawn(handlers: TrayHandlers) -> TrayHandle {
        let slot = Arc::new(TooltipSlot::default());
        let (hwnd, join) = spawn_thread(handlers, Arc::clone(&slot));
        TrayHandle {
            inner: Arc::new(Inner {
                hwnd,
                slot,
                join: Mutex::new(join),
            }),
        }
    }
}

#[cfg(windows)]
fn spawn_thread(handlers: TrayHandlers, slot: Arc<TooltipSlot>) -> (isize, Option<JoinHandle<()>>) {
    let (ready_tx, ready_rx) = mpsc::channel::<isize>();
    let handlers = Arc::new(handlers);
    let spawned = thread::Builder::new()
        .name("tray".to_owned())
        .spawn(move || win::run(handlers, slot, ready_tx));
    match spawned {
        Ok(join) => {
            let hwnd = ready_rx.recv().unwrap_or(0);
            (hwnd, Some(join))
        }
        Err(e) => {
            logging::error(&format!("cannot start the tray thread: {e}"));
            (0, None)
        }
    }
}

#[cfg(not(windows))]
fn spawn_thread(
    handlers: TrayHandlers,
    _slot: Arc<TooltipSlot>,
) -> (isize, Option<JoinHandle<()>>) {
    // No shell off Windows. The handlers are dropped, so nothing can call them.
    drop(handlers);
    (0, None)
}

/// Handle to the running tray. Cheap to clone. The icon is removed when the
/// last clone is dropped or [`TrayHandle::shutdown`] is called.
#[derive(Clone)]
pub struct TrayHandle {
    inner: Arc<Inner>,
}

struct Inner {
    /// Hidden window handle as an integer. Zero when there is no tray.
    hwnd: isize,
    slot: Arc<TooltipSlot>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl TrayHandle {
    /// Sets the tooltip. Updates are applied at most every [`TOOLTIP_MIN_INTERVAL`]
    /// and skipped when the text has not changed.
    pub fn set_tooltip(&self, text: &str) {
        {
            let mut last = lock(&self.inner.slot.last_requested);
            if last.as_str() == text {
                return;
            }
            *last = text.to_owned();
        }
        *lock(&self.inner.slot.pending) = Some(text.to_owned());
        post_tooltip(self.inner.hwnd);
    }

    /// Removes the icon, closes the tray window, and waits for the tray thread.
    /// Idempotent. When the confirm dialog is open the wait is skipped, since
    /// the thread is stuck in a modal loop and the caller is about to exit.
    pub fn shutdown(&self) {
        shutdown_inner(&self.inner);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        shutdown_inner(self);
    }
}

fn shutdown_inner(inner: &Inner) {
    if inner.hwnd != 0 {
        remove_icon(inner.hwnd);
        close_window(inner.hwnd);
    }
    if DIALOG_OPEN.load(Ordering::SeqCst) {
        return;
    }
    let handle = lock(&inner.join).take();
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

#[cfg(windows)]
fn post_tooltip(hwnd: isize) {
    if hwnd != 0 {
        win::post(hwnd, TOOLTIP_MESSAGE);
    }
}

#[cfg(not(windows))]
fn post_tooltip(_hwnd: isize) {}

#[cfg(windows)]
fn remove_icon(hwnd: isize) {
    win::remove_icon(hwnd);
}

#[cfg(not(windows))]
fn remove_icon(_hwnd: isize) {}

#[cfg(windows)]
fn close_window(hwnd: isize) {
    win::close_window(hwnd);
}

#[cfg(not(windows))]
fn close_window(_hwnd: isize) {}

#[cfg(windows)]
mod win {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::rc::Rc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Instant;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, LoadIconW,
        MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
        SetForegroundWindow, SetMenuDefaultItem, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
        IDI_APPLICATION, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST,
        MB_YESNO, MF_STRING, MSG, SM_CXSMICON, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_STYLE,
        WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL,
        WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    };

    use crate::branding;
    use crate::logging;

    use super::{
        lock, menu_items, MenuItem, TooltipSlot, TrayHandlers, ADD_RETRY_DELAY_MS, ADD_RETRY_LIMIT,
        APP_TOOLTIP, CALLBACK_MESSAGE, CONFIRM_EXIT_TEXT, DIALOG_OPEN, TIMER_ADD_RETRY,
        TIMER_TOOLTIP, TOOLTIP_MESSAGE, TOOLTIP_MIN_INTERVAL, TRAY_CLASS, TRAY_UID,
    };

    /// `NIN_KEYSELECT` (`WM_USER + 1`). windows 0.58 does not export it.
    const NIN_KEYSELECT_EVENT: u32 = 0x0401;

    /// Per-thread state the window procedure needs. Only the tray thread touches it.
    struct Ctx {
        hwnd: HWND,
        /// Icon handle, owned when `icon_owned` (destroyed on window teardown).
        icon: isize,
        icon_owned: bool,
        handlers: Arc<TrayHandlers>,
        slot: Arc<TooltipSlot>,
        /// Registered `TaskbarCreated` message id.
        taskbar_message: u32,
        add_attempts: Cell<u32>,
        /// Tooltip text currently shown.
        applied: RefCell<String>,
        last_applied: Cell<Option<Instant>>,
        tooltip_timer_armed: Cell<bool>,
    }

    thread_local! {
        static CTX: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
    }

    fn current() -> Option<Rc<Ctx>> {
        CTX.with(|cell| cell.borrow().clone())
    }

    /// Thread body: creates the window, adds the icon, runs the message loop until
    /// the window is destroyed. `ready` receives the window handle, or 0 on failure.
    pub(super) fn run(handlers: Arc<TrayHandlers>, slot: Arc<TooltipSlot>, ready: Sender<isize>) {
        // SAFETY: creates the hidden window on this thread; it is destroyed by the
        // default WM_CLOSE handler on this same thread.
        let hwnd = match unsafe { create_window() } {
            Ok(hwnd) => hwnd,
            Err(e) => {
                logging::error(&format!("cannot create the tray window: {e}"));
                let _ = ready.send(0);
                return;
            }
        };
        let _ = ready.send(hwnd.0 as isize);

        let (icon, icon_owned) = match branding::load_tray_icon(icon_size()) {
            Some(handle) => (handle, true),
            None => {
                logging::warn("tray: embedded icon failed to load; using the system icon");
                (fallback_icon(), false)
            }
        };
        // SAFETY: the argument is a static NUL-terminated wide string.
        let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

        let ctx = Rc::new(Ctx {
            hwnd,
            icon,
            icon_owned,
            handlers,
            slot,
            taskbar_message,
            add_attempts: Cell::new(0),
            applied: RefCell::new(String::new()),
            last_applied: Cell::new(None),
            tooltip_timer_armed: Cell::new(false),
        });
        CTX.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&ctx)));

        // SAFETY: the context is set and the window exists on this thread.
        if !unsafe { add_icon(&ctx) } {
            logging::warn("tray icon not added yet; retrying every 2 s");
            // SAFETY: the window is ours and alive.
            unsafe { arm_timer(hwnd, TIMER_ADD_RETRY, ADD_RETRY_DELAY_MS) };
        }

        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer. A zero result means WM_QUIT,
        // a negative one means an error; both end the loop.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            // SAFETY: `msg` was filled in by GetMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        CTX.with(|cell| *cell.borrow_mut() = None);
    }

    /// Posts a message to the tray window. Safe from any thread.
    pub(super) fn post(hwnd: isize, msg: u32) {
        // SAFETY: PostMessageW only queues the message and never dereferences anything.
        unsafe {
            let _ = PostMessageW(window(hwnd), msg, WPARAM(0), LPARAM(0));
        }
    }

    /// Asks the tray window to close. The default handler destroys it, which ends the thread.
    pub(super) fn close_window(hwnd: isize) {
        post(hwnd, WM_CLOSE);
    }

    /// Removes the icon. Callable from any thread; the shell keys on window and id.
    pub(super) fn remove_icon(hwnd: isize) {
        let nid = notify_data(window(hwnd));
        // SAFETY: `nid` is a valid NOTIFYICONDATAW naming our own icon.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    /// Shows the native confirm dialog. The default button is No.
    pub(super) fn confirm_dialog() -> bool {
        let text = wide(CONFIRM_EXIT_TEXT);
        let title = wide("ClipCheck");
        DIALOG_OPEN.store(true, Ordering::SeqCst);
        // SAFETY: both buffers are NUL-terminated and live for the call. No owner window.
        let answer = unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND | MB_DEFBUTTON2,
            )
        };
        DIALOG_OPEN.store(false, Ordering::SeqCst);
        answer == IDYES
    }

    fn window(hwnd: isize) -> HWND {
        HWND(hwnd as *mut c_void)
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn icon_size() -> u32 {
        // SAFETY: GetSystemMetrics has no pointer arguments.
        let px = unsafe { GetSystemMetrics(SM_CXSMICON) };
        u32::try_from(px).ok().filter(|v| *v > 0).unwrap_or(16)
    }

    fn fallback_icon() -> isize {
        // SAFETY: loads a stock system icon; no pointers besides a constant resource id.
        unsafe { LoadIconW(None, IDI_APPLICATION) }
            .map(|h| h.0 as isize)
            .unwrap_or(0)
    }

    /// Creates the hidden top-level window. Top-level, not message-only, so it
    /// receives the `TaskbarCreated` broadcast.
    ///
    /// # Safety
    /// Must run on the thread that will pump the window's messages.
    unsafe fn create_window() -> windows::core::Result<HWND> {
        let module = GetModuleHandleW(None)?;
        let instance = HINSTANCE(module.0);
        let class = wide(TRAY_CLASS);
        let title = wide(APP_TOOLTIP);
        let class_def = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // Zero means the class already exists in this process (a second tray).
        // CreateWindowExW then still succeeds, so the result is not checked.
        let _ = RegisterClassW(&class_def);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )
    }

    /// Window procedure. Runs on the tray thread only.
    ///
    /// # Safety
    /// Called by the system with a valid window handle and message parameters.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let Some(ctx) = current() else {
            // SAFETY: forwarding the same arguments the system gave us.
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        if ctx.taskbar_message != 0 && msg == ctx.taskbar_message {
            // Explorer restarted and dropped the icon. Add it again.
            add_icon(&ctx);
            return LRESULT(0);
        }
        match msg {
            CALLBACK_MESSAGE => {
                // With NOTIFYICON_VERSION_4 the event code is in the low word of lparam.
                let event = (lparam.0 & 0xFFFF) as u32;
                match event {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | NIN_KEYSELECT_EVENT => {
                        (ctx.handlers.open_ui)();
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&ctx),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                dispatch((wparam.0 & 0xFFFF) as u16, &ctx);
                LRESULT(0)
            }
            WM_TIMER => {
                on_timer(&ctx, wparam.0);
                LRESULT(0)
            }
            TOOLTIP_MESSAGE => {
                apply_tooltip(&ctx);
                LRESULT(0)
            }
            WM_DESTROY => {
                remove_icon_of(&ctx);
                if ctx.icon_owned {
                    crate::branding::destroy_icon(ctx.icon);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Runs the action for a menu id or WM_COMMAND id. An id that is not a menu
    /// item is ignored, so it can never reach the exit path.
    fn dispatch(id: u16, ctx: &Ctx) {
        match MenuItem::from_id(id) {
            Some(MenuItem::Open) => (ctx.handlers.open_ui)(),
            Some(MenuItem::Exit) => (ctx.handlers.request_exit)(),
            None => logging::warn(&format!("tray: ignored unknown menu id {id}")),
        }
    }

    /// Shows the right-click menu and runs the chosen action.
    fn show_menu(ctx: &Ctx) {
        // SAFETY: every call below takes handles this thread owns (the menu is
        // created and destroyed here, the window is ours). Buffers outlive their calls.
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                logging::warn("tray: cannot create the menu");
                return;
            };
            for (item, label) in menu_items() {
                let text = wide(label);
                // AppendMenuW copies the string.
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    usize::from(item.id()),
                    PCWSTR(text.as_ptr()),
                );
            }
            let _ = SetMenuDefaultItem(menu, u32::from(MenuItem::Open.id()), 0);
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            // Needed so the menu dismisses when the user clicks elsewhere.
            let _ = SetForegroundWindow(ctx.hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                point.x,
                point.y,
                0,
                ctx.hwnd,
                None,
            );
            // The documented tray-menu workaround: a WM_NULL after the menu stops
            // the next click on the icon from being swallowed.
            let _ = PostMessageW(ctx.hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            if chosen.0 > 0 {
                dispatch(chosen.0 as u16, ctx);
            }
        }
    }

    /// Adds the icon with the current tooltip. Returns false if the shell refused.
    ///
    /// # Safety
    /// `ctx.hwnd` must be the live tray window on this thread.
    unsafe fn add_icon(ctx: &Ctx) -> bool {
        let mut nid = notify_data(ctx.hwnd);
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = CALLBACK_MESSAGE;
        nid.hIcon = HICON(ctx.icon as *mut c_void);
        nid.szTip = tip_units(&shown_tooltip(ctx));
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            return false;
        }
        // Version 4 gives NIN_SELECT for left clicks and WM_CONTEXTMENU for right clicks.
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        true
    }

    fn remove_icon_of(ctx: &Ctx) {
        remove_icon(ctx.hwnd.0 as isize);
    }

    fn shown_tooltip(ctx: &Ctx) -> String {
        let shown = ctx.applied.borrow();
        if shown.is_empty() {
            APP_TOOLTIP.to_owned()
        } else {
            shown.clone()
        }
    }

    /// Applies the pending tooltip now, or arms a timer when the last update was too recent.
    ///
    /// # Safety
    /// Called on the tray thread with the live window.
    unsafe fn apply_tooltip(ctx: &Ctx) {
        if lock(&ctx.slot.pending).is_none() {
            return;
        }
        if let Some(last) = ctx.last_applied.get() {
            let elapsed = last.elapsed();
            if elapsed < TOOLTIP_MIN_INTERVAL {
                if !ctx.tooltip_timer_armed.get() {
                    let wait = (TOOLTIP_MIN_INTERVAL - elapsed).as_millis().max(1);
                    let wait = u32::try_from(wait).unwrap_or(5_000);
                    arm_timer(ctx.hwnd, TIMER_TOOLTIP, wait);
                    ctx.tooltip_timer_armed.set(true);
                }
                return;
            }
        }
        let Some(text) = lock(&ctx.slot.pending).take() else {
            return;
        };
        let mut nid = notify_data(ctx.hwnd);
        nid.uFlags = NIF_TIP;
        nid.szTip = tip_units(&text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        ctx.last_applied.set(Some(Instant::now()));
        *ctx.applied.borrow_mut() = text;
    }

    /// Handles the retry and tooltip timers.
    ///
    /// # Safety
    /// Called on the tray thread with the live window.
    unsafe fn on_timer(ctx: &Ctx, id: usize) {
        match id {
            TIMER_ADD_RETRY => {
                let _ = KillTimer(ctx.hwnd, id);
                if add_icon(ctx) {
                    logging::info("tray icon added");
                    return;
                }
                let attempts = ctx.add_attempts.get() + 1;
                ctx.add_attempts.set(attempts);
                if attempts <= ADD_RETRY_LIMIT {
                    arm_timer(ctx.hwnd, TIMER_ADD_RETRY, ADD_RETRY_DELAY_MS);
                } else {
                    logging::error("tray icon could not be added; the daemon runs without it");
                }
            }
            TIMER_TOOLTIP => {
                let _ = KillTimer(ctx.hwnd, id);
                ctx.tooltip_timer_armed.set(false);
                apply_tooltip(ctx);
            }
            _ => {}
        }
    }

    /// # Safety
    /// `hwnd` must be a live window owned by this thread.
    unsafe fn arm_timer(hwnd: HWND, id: usize, ms: u32) {
        let _ = SetTimer(hwnd, id, ms, None);
    }

    fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>())
                .expect("NOTIFYICONDATAW size fits in u32"),
            hWnd: hwnd,
            uID: TRAY_UID,
            ..Default::default()
        }
    }

    /// Copies `text` into the fixed tooltip buffer, truncated to fit with a NUL.
    fn tip_units(text: &str) -> [u16; 128] {
        let mut buf = [0u16; 128];
        for (slot, unit) in buf.iter_mut().take(127).zip(text.encode_utf16()) {
            *slot = unit;
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_target_prefers_sibling_when_launched_as_daemon() {
        let daemon = Path::new(r"C:\app\clipcheckd.exe");
        assert_eq!(
            ui_target(daemon, true),
            PathBuf::from(r"C:\app\ClipCheck.exe")
        );
        assert_eq!(ui_target(daemon, false), daemon.to_path_buf());
    }

    #[test]
    fn ui_target_is_current_exe_inside_clipcheck() {
        let app = Path::new(r"C:\app\ClipCheck.exe");
        assert_eq!(ui_target(app, true), app.to_path_buf());
    }

    #[test]
    fn tooltip_formats_elapsed_and_game() {
        assert_eq!(tooltip_text(None), "ClipCheck");
        assert_eq!(
            tooltip_text(Some((Some("Hades"), 42 * 60 + 17))),
            "ClipCheck \u{2014} recording Hades 0:42:17"
        );
        assert_eq!(
            tooltip_text(Some((None, 3600 + 5))),
            "ClipCheck \u{2014} recording 1:00:05"
        );
    }
}
