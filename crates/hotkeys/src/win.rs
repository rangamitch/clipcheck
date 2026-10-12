//! Windows hotkey backend: a non-swallowing low-level keyboard hook on a
//! dedicated message thread.
//!
//! Each [`HotkeyService`] owns one thread (`hotkey-msg`) that creates a
//! message-only window, installs a `WH_KEYBOARD_LL` hook, and pumps its message
//! queue. The hook only observes key events. It always calls `CallNextHookEx`,
//! so no key is ever swallowed: a ClipCheck chord still reaches the foreground
//! application and the game, and keeps its normal function there.
//!
//! Matching and tap/long decisions live in the pure `hook_state` module. The
//! hook callback does only that cheap state update and an mpsc send; the long-press
//! timer runs as `WM_TIMER` on the same thread.
//!
//! Anti-cheat note: low-level keyboard hooks are a standard, documented Windows
//! mechanism, but some anti-cheat systems flag any global input hook. See
//! `docs/design/05-replay-buffer-and-hotkeys.md`.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    PostThreadMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, HWND_MESSAGE,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::gesture::ChordId;
use crate::hook_state::{Fired, HoldEvent, HookState};
use crate::{Chord, HotkeyAction, PressKind, Registry};

/// Thread message posted by [`HotkeyService::apply`]; `lParam` owns an [`ApplyRequest`].
const WM_APP_APPLY: u32 = WM_APP + 1;

/// How long [`HotkeyService::apply`] waits for the service thread to reply
/// before it cancels the request (if the thread has not started it yet).
const APPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Hold threshold used by [`HotkeyService::start`]. The daemon passes the core
/// `LONG_PRESS_MS` (400) to [`HotkeyService::start_with_threshold`] instead.
pub const DEFAULT_LONG_PRESS_MS: u32 = 400;

/// Virtual-key codes used for mouse buttons in chords: middle, back (XBUTTON1)
/// and forward (XBUTTON2). Left and right buttons are not hotkey keys.
const VK_MOUSE3: u32 = 0x04;
const VK_MOUSE4: u32 = 0x05;
const VK_MOUSE5: u32 = 0x06;

/// `XBUTTON1` / `XBUTTON2` values in the high word of `MSLLHOOKSTRUCT.mouseData`.
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// Timer id that drives long-press ticks while a hold is pending.
const GESTURE_TIMER: usize = 1;

/// Tick interval in milliseconds, used only while a hold is pending.
const GESTURE_POLL_MS: u32 = 10;

/// Value of the suspension atomic when the UI is not capturing keys.
const NOT_SUSPENDED: u64 = u64::MAX;

/// Monotonic clock shared by the hook thread and [`HotkeyService::set_suspended`].
static CLOCK: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the first call in this process.
fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// A hotkey press forwarded from the message thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEvent {
    /// The action bound to the pressed chord.
    pub action: HotkeyAction,
    /// `true` when the chord fired (tap, long press, or the key going down for
    /// [`HotkeyAction::PushToTalk`]); `false` only for the release of a push-to-talk chord.
    pub pressed: bool,
}

/// Why a binding could not be registered.
///
/// The keyboard-hook backend never registers chords with the OS, so it never
/// produces [`FailureReason::TakenByAnotherApp`]. The variant is kept for API
/// compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// Another application owns the chord (`RegisterHotKey` error 1409). Not produced by the hook backend.
    TakenByAnotherApp,
    /// Any other Win32 error code.
    Os(u32),
}

/// One binding that could not be registered. The hook backend returns none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterFailure {
    /// The chord that was refused.
    pub chord: Chord,
    /// The action the chord was bound to.
    pub action: HotkeyAction,
    /// Why it was refused.
    pub reason: FailureReason,
}

impl fmt::Display for RegisterFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            FailureReason::TakenByAnotherApp => {
                write!(f, "{} is already used by another application", self.chord)
            }
            FailureReason::Os(code) => {
                write!(
                    f,
                    "{} could not be registered (Win32 error {})",
                    self.chord, code
                )
            }
        }
    }
}

impl std::error::Error for RegisterFailure {}

/// Why [`HotkeyService::apply`] could not run. Refused chords are not an error;
/// they come back as `Ok(failures)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyError {
    /// The service thread is no longer running, or it did not start the
    /// request within the timeout. In the timeout case the request was
    /// cancelled before it ran, so the active bindings are unchanged.
    Stopped,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplyError::Stopped => f.write_str("hotkey service is not running"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Infrastructure failure that prevents the hotkey service from existing at
/// all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The `hotkey-msg` thread could not be spawned.
    Spawn(String),
    /// The message-only window could not be created (Win32 error code).
    Window(u32),
    /// The low-level keyboard hook could not be installed (Win32 error code).
    Hook(u32),
    /// The message thread exited before reporting readiness.
    NoReply,
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Spawn(e) => write!(f, "hotkey thread could not be started: {e}"),
            StartError::Window(code) => {
                write!(f, "hotkey window could not be created (Win32 error {code})")
            }
            StartError::Hook(code) => {
                write!(
                    f,
                    "keyboard hook could not be installed (Win32 error {code})"
                )
            }
            StartError::NoReply => f.write_str("hotkey thread exited during startup"),
        }
    }
}

impl std::error::Error for StartError {}

/// Running hotkey service. Dropping it (or calling [`HotkeyService::stop`])
/// removes the keyboard hook and joins the message thread.
#[derive(Debug)]
pub struct HotkeyService {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
    /// [`now_ms`] when the UI last asked for suspension, or [`NOT_SUSPENDED`].
    suspended: Arc<AtomicU64>,
}

/// Startup handshake sent from the message thread to [`HotkeyService::start`].
enum Startup {
    /// Hook is installed; carries the thread id for posting.
    Ready { thread_id: u32 },
    /// Setup failed; the thread has already torn down what it created.
    Failed(StartError),
}

/// Payload of a `WM_APP_APPLY` message. Created with `Box::into_raw`.
struct ApplyRequest {
    registry: Registry,
    reply: Sender<Vec<RegisterFailure>>,
    gate: Arc<ApplyGate>,
}

/// Lifecycle of one apply request. Starts `Pending`; the message thread moves
/// it to `Claimed` before touching the bindings, and a caller that gave up
/// moves it to `Cancelled`. Only one of those transitions can happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyState {
    Pending,
    Claimed,
    Cancelled,
}

/// Arbitrates between the caller's timeout and the message thread. The lock
/// is held only for the state change, never while waiting or applying.
#[derive(Debug)]
struct ApplyGate(Mutex<ApplyState>);

impl ApplyGate {
    fn new() -> Self {
        ApplyGate(Mutex::new(ApplyState::Pending))
    }

    fn state(&self) -> MutexGuard<'_, ApplyState> {
        // A poisoned lock still holds a valid state value, so keep using it.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Message thread: returns `true` if the request may run now. Returns
    /// `false` if the caller already cancelled it.
    fn claim(&self) -> bool {
        let mut state = self.state();
        if *state == ApplyState::Pending {
            *state = ApplyState::Claimed;
            true
        } else {
            false
        }
    }

    /// Caller after a timeout: returns `true` if the request was cancelled
    /// before the message thread claimed it. Returns `false` if it already runs.
    fn cancel(&self) -> bool {
        let mut state = self.state();
        if *state == ApplyState::Pending {
            *state = ApplyState::Cancelled;
            true
        } else {
            false
        }
    }
}

/// One distinct chord with its tap and/or long action. A chord has both only
/// when two clip hotkeys share it (one tap, one long).
#[derive(Debug, Clone)]
struct Slot {
    id: ChordId,
    chord: Chord,
    tap: Option<HotkeyAction>,
    long: Option<HotkeyAction>,
    /// Push-to-talk action: a hold chord that reports press and release.
    hold: Option<HotkeyAction>,
}

/// Groups the enabled bindings of `registry` into one slot per chord, with ids 1..=n.
fn build_slots(registry: &Registry) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    for binding in registry.enabled() {
        let index = match slots.iter().position(|s| s.chord == binding.chord) {
            Some(index) => index,
            None => {
                slots.push(Slot {
                    id: slots.len() as ChordId + 1,
                    chord: binding.chord,
                    tap: None,
                    long: None,
                    hold: None,
                });
                slots.len() - 1
            }
        };
        let slot = &mut slots[index];
        if matches!(binding.action, HotkeyAction::PushToTalk) {
            slot.hold = Some(binding.action.clone());
            continue;
        }
        match binding.press {
            PressKind::Tap => slot.tap = Some(binding.action.clone()),
            PressKind::Long => slot.long = Some(binding.action.clone()),
        }
    }
    slots
}

/// Everything the hook thread needs. Lives in [`CTX`] for the thread's lifetime.
struct HookCtx {
    hwnd: HWND,
    hook: HHOOK,
    /// Low-level mouse hook, installed only while a binding uses `Mouse3`/`Mouse4`/`Mouse5`.
    /// Null when not installed.
    mouse_hook: HHOOK,
    tx: Sender<HotkeyEvent>,
    long_press_ms: u32,
    slots: Vec<Slot>,
    state: HookState,
    suspended: Arc<AtomicU64>,
    timer: bool,
}

thread_local! {
    /// The hook callback and the message loop both run on the `hotkey-msg`
    /// thread, so this cell is only touched there.
    static CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

impl HookCtx {
    fn new(
        hwnd: HWND,
        tx: Sender<HotkeyEvent>,
        long_press_ms: u32,
        suspended: Arc<AtomicU64>,
        registry: &Registry,
    ) -> HookCtx {
        let slots = build_slots(registry);
        let state = make_state(long_press_ms, &slots);
        HookCtx {
            hwnd,
            hook: HHOOK(std::ptr::null_mut()),
            mouse_hook: HHOOK(std::ptr::null_mut()),
            tx,
            long_press_ms,
            slots,
            state,
            suspended,
            timer: false,
        }
    }

    /// Copies the suspension atomic into the pure state. `None` means not suspended.
    fn sync_suspension(&mut self) {
        let value = self.suspended.load(Ordering::Acquire);
        self.state
            .set_suspension((value != NOT_SUSPENDED).then_some(value));
    }

    /// Handles one key event from the hook callback.
    fn key(&mut self, vk: u32, down: bool, flags: u32) {
        self.sync_suspension();
        let fired = self.state.on_key(vk, down, flags, now_ms());
        self.dispatch(fired);
    }

    /// Handles one mouse-button event from the mouse hook callback. `vk` is one
    /// of [`VK_MOUSE3`], [`VK_MOUSE4`], [`VK_MOUSE5`].
    fn mouse(&mut self, vk: u32, down: bool) {
        self.sync_suspension();
        let fired = self.state.on_mouse(vk, down, now_ms());
        self.dispatch(fired);
    }

    /// Installs the mouse hook when an enabled binding uses a mouse-button key,
    /// and removes it when none does. A failed install is skipped silently: the
    /// keyboard service keeps running, and the install is retried on the next
    /// `replace`.
    fn sync_mouse_hook(&mut self) {
        let want = self.slots.iter().any(|s| is_mouse_vk(s.chord.vk_code()));
        if want && self.mouse_hook.0.is_null() {
            if let Ok(hook) = install_mouse_hook() {
                self.mouse_hook = hook;
            }
        } else if !want {
            self.unhook_mouse();
        }
    }

    /// Removes the mouse hook if it is installed.
    fn unhook_mouse(&mut self) {
        if !self.mouse_hook.0.is_null() {
            // SAFETY: `mouse_hook` was returned by SetWindowsHookExW on this
            // thread and is cleared here, so it is removed once.
            let _ = unsafe { UnhookWindowsHookEx(self.mouse_hook) };
            self.mouse_hook = HHOOK(std::ptr::null_mut());
        }
    }

    /// Handles one `WM_TIMER` tick. Also releases push-to-talk holds whose key
    /// is no longer physically down, in case a key-up was missed.
    fn tick(&mut self) {
        self.sync_suspension();
        let fired = self.state.tick(now_ms());
        self.state.check_holds(|vk| {
            // SAFETY: GetAsyncKeyState has no preconditions.
            let state = unsafe { GetAsyncKeyState(vk as i32) };
            (state as u16) & 0x8000 != 0
        });
        self.dispatch(fired);
    }

    /// Sends the action for each fired chord and each push-to-talk press or
    /// release, then keeps the timer in step with pending holds.
    fn dispatch(&mut self, fired: Vec<Fired>) {
        for Fired { id, press } in fired {
            let Some(slot) = self.slots.iter().find(|s| s.id == id) else {
                continue;
            };
            let action = match press {
                PressKind::Tap => slot.tap.clone(),
                PressKind::Long => slot.long.clone(),
            };
            if let Some(action) = action {
                let _ = self.tx.send(HotkeyEvent {
                    action,
                    pressed: true,
                });
            }
        }
        self.dispatch_holds();
        self.sync_timer();
    }

    /// Sends a push-to-talk event for each hold press or release queued in the state.
    fn dispatch_holds(&mut self) {
        for HoldEvent { id, pressed } in self.state.take_hold_events() {
            let action = self
                .slots
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.hold.clone());
            if let Some(action) = action {
                let _ = self.tx.send(HotkeyEvent { action, pressed });
            }
        }
    }

    /// Starts the tick timer while a hold is pending or a push-to-talk hold is
    /// down, and stops it when neither is.
    fn sync_timer(&mut self) {
        let want = self.state.is_pending() || self.state.has_active_holds();
        if want && !self.timer {
            // SAFETY: hwnd is the live message window owned by this thread. A
            // null callback makes WM_TIMER arrive as a normal message.
            let id = unsafe { SetTimer(self.hwnd, GESTURE_TIMER, GESTURE_POLL_MS, None) };
            self.timer = id != 0;
        } else if !want && self.timer {
            // SAFETY: the timer was set on this hwnd by sync_timer.
            let _ = unsafe { KillTimer(self.hwnd, GESTURE_TIMER) };
            self.timer = false;
        }
    }

    /// Replaces the bindings. Active push-to-talk holds are released first, using
    /// the old slots so the release reaches the right action. Pending taps and
    /// long presses are dropped with the old state.
    fn replace(&mut self, registry: &Registry) {
        self.state.release_all_holds();
        self.dispatch_holds();
        self.slots = build_slots(registry);
        self.state = make_state(self.long_press_ms, &self.slots);
        self.sync_mouse_hook();
        self.sync_timer();
    }
}

/// True for the virtual-key codes of the mouse buttons a chord may use.
fn is_mouse_vk(vk: u32) -> bool {
    matches!(vk, VK_MOUSE3 | VK_MOUSE4 | VK_MOUSE5)
}

fn make_state(long_press_ms: u32, slots: &[Slot]) -> HookState {
    // A push-to-talk slot is a hold chord: it gets no tap or long gesture.
    let entries: Vec<(ChordId, Chord, bool, bool)> = slots
        .iter()
        .map(|s| {
            let is_hold = s.hold.is_some();
            (
                s.id,
                s.chord,
                !is_hold && s.tap.is_some(),
                !is_hold && s.long.is_some(),
            )
        })
        .collect();
    let holds: Vec<ChordId> = slots
        .iter()
        .filter(|s| s.hold.is_some())
        .map(|s| s.id)
        .collect();
    let mut state = HookState::new(u64::from(long_press_ms), &entries);
    state.set_holds(&holds);
    state
}

/// Runs `f` on the thread's hook context, if one is set. Skips the call if the
/// cell is already borrowed, so the callback never panics.
fn with_ctx(f: impl FnOnce(&mut HookCtx)) {
    CTX.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(ctx) = guard.as_mut() {
                f(ctx);
            }
        }
    });
}

impl HotkeyService {
    /// Starts the keyboard hook and forwards chord presses to `tx`.
    ///
    /// The hook observes keys without swallowing them. The returned failure
    /// list is always empty; it is kept so existing callers keep compiling.
    ///
    /// `Err` is returned only when the service itself cannot be created, such as
    /// when the keyboard hook cannot be installed.
    pub fn start(
        registry: &Registry,
        tx: Sender<HotkeyEvent>,
    ) -> Result<(HotkeyService, Vec<RegisterFailure>), StartError> {
        Self::start_with_threshold(registry, tx, DEFAULT_LONG_PRESS_MS)
    }

    /// Like [`HotkeyService::start`], with the hold threshold in milliseconds used for
    /// `Long` bindings. A clip chord that has both a tap and a long binding fires the tap on
    /// release when the hold is shorter than the threshold, and fires the long action once the
    /// hold reaches it. A long action fires while the key is still held.
    pub fn start_with_threshold(
        registry: &Registry,
        tx: Sender<HotkeyEvent>,
        long_press_ms: u32,
    ) -> Result<(HotkeyService, Vec<RegisterFailure>), StartError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Startup>();
        let thread_registry = registry.clone();
        let suspended = Arc::new(AtomicU64::new(NOT_SUSPENDED));
        let thread_suspended = Arc::clone(&suspended);
        let handle = std::thread::Builder::new()
            .name("hotkey-msg".into())
            .spawn(move || {
                message_thread(
                    thread_registry,
                    tx,
                    ready_tx,
                    long_press_ms,
                    thread_suspended,
                )
            })
            .map_err(|e| StartError::Spawn(e.to_string()))?;

        match ready_rx.recv() {
            Ok(Startup::Ready { thread_id }) => Ok((
                HotkeyService {
                    thread_id,
                    handle: Some(handle),
                    suspended,
                },
                Vec::new(),
            )),
            Ok(Startup::Failed(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                let _ = handle.join();
                Err(StartError::NoReply)
            }
        }
    }

    /// Replaces the active bindings with the enabled bindings of `registry`.
    ///
    /// Blocks until the message thread replies. The reply is always an empty
    /// list, because the hook never refuses a chord.
    ///
    /// `Err(ApplyError::Stopped)` means either the service thread is gone, or
    /// the thread did not start the request within [`APPLY_TIMEOUT`]. In the
    /// second case the request is cancelled and never runs, so the active
    /// bindings are unchanged. If the thread has already started the request,
    /// `apply` waits for its real result instead of reporting an error.
    pub fn apply(&self, registry: Registry) -> Result<Vec<RegisterFailure>, ApplyError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let gate = Arc::new(ApplyGate::new());
        let request = Box::into_raw(Box::new(ApplyRequest {
            registry,
            reply: reply_tx,
            gate: Arc::clone(&gate),
        }));

        // SAFETY: PostThreadMessageW only queues the message; lParam is a
        // pointer-sized value that the service thread reclaims exactly once.
        let posted = unsafe {
            PostThreadMessageW(
                self.thread_id,
                WM_APP_APPLY,
                WPARAM(0),
                LPARAM(request as isize),
            )
        };
        if posted.is_err() {
            // SAFETY: the post failed, so the thread never received this
            // pointer and we still own it.
            drop(unsafe { Box::from_raw(request) });
            return Err(ApplyError::Stopped);
        }

        match reply_rx.recv_timeout(APPLY_TIMEOUT) {
            Ok(failures) => Ok(failures),
            Err(RecvTimeoutError::Timeout) => {
                if gate.cancel() {
                    // Not started: the thread will drop the request unrun.
                    Err(ApplyError::Stopped)
                } else {
                    // Already claimed and being applied; wait for its real reply.
                    reply_rx.recv().map_err(|_| ApplyError::Stopped)
                }
            }
            Err(RecvTimeoutError::Disconnected) => Err(ApplyError::Stopped),
        }
    }

    /// Tells the hook to ignore chords (while the UI captures a new hotkey) or
    /// to resume. Modifier tracking continues during a suspension. A suspension
    /// expires on its own after 60 s, so a lost resume cannot disable hotkeys for good.
    pub fn set_suspended(&self, suspended: bool) {
        let value = if suspended { now_ms() } else { NOT_SUSPENDED };
        self.suspended.store(value, Ordering::Release);
    }

    /// Removes the keyboard hook, stops the message thread and waits for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: posting WM_QUIT to a thread id only queues a message.
            // If the thread already exited the post fails and join returns.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = handle.join();
        }
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Body of the `hotkey-msg` thread: window, hook, message pump.
fn message_thread(
    registry: Registry,
    tx: Sender<HotkeyEvent>,
    ready: Sender<Startup>,
    long_press_ms: u32,
    suspended: Arc<AtomicU64>,
) {
    let hwnd = match create_message_window() {
        Ok(hwnd) => hwnd,
        Err(code) => {
            let _ = ready.send(Startup::Failed(StartError::Window(code)));
            return;
        }
    };

    let ctx = HookCtx::new(hwnd, tx, long_press_ms, suspended, &registry);
    CTX.with(|cell| *cell.borrow_mut() = Some(ctx));

    let hook = match install_hook() {
        Ok(hook) => hook,
        Err(code) => {
            CTX.with(|cell| *cell.borrow_mut() = None);
            destroy_window(hwnd);
            let _ = ready.send(Startup::Failed(StartError::Hook(code)));
            return;
        }
    };
    with_ctx(|ctx| {
        ctx.hook = hook;
        ctx.sync_mouse_hook();
    });

    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Startup::Ready { thread_id });

    pump();

    // Teardown: the hooks must go before the window and the context. Active
    // push-to-talk holds are released first so a held key cannot stay stuck.
    with_ctx(|ctx| {
        ctx.state.release_all_holds();
        ctx.dispatch_holds();
        ctx.unhook_mouse();
        // SAFETY: `hook` was returned by SetWindowsHookExW on this thread and is removed once.
        let _ = unsafe { UnhookWindowsHookEx(ctx.hook) };
        ctx.hook = HHOOK(std::ptr::null_mut());
    });
    CTX.with(|cell| *cell.borrow_mut() = None);
    destroy_window(hwnd);
}

/// Installs the `WH_KEYBOARD_LL` hook on this thread. Returns the Win32 code on failure.
fn install_hook() -> Result<HHOOK, u32> {
    // SAFETY: GetModuleHandleW(None) returns the handle of the running executable.
    let module = unsafe { GetModuleHandleW(None) }
        .map(|m| HINSTANCE(m.0))
        .unwrap_or(HINSTANCE(std::ptr::null_mut()));
    // SAFETY: keyboard_proc has the HOOKPROC signature. The module handle
    // is the running executable and dwThreadId 0 makes a global hook.
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) }
        .map_err(|e| win32_code(&e))
}

/// Installs the `WH_MOUSE_LL` hook on this thread. Returns the Win32 code on failure.
fn install_mouse_hook() -> Result<HHOOK, u32> {
    // SAFETY: GetModuleHandleW(None) returns the handle of the running executable.
    let module = unsafe { GetModuleHandleW(None) }
        .map(|m| HINSTANCE(m.0))
        .unwrap_or(HINSTANCE(std::ptr::null_mut()));
    // SAFETY: mouse_proc has the HOOKPROC signature. The module handle is the
    // running executable and dwThreadId 0 makes a global hook.
    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0) }
        .map_err(|e| win32_code(&e))
}

/// Maps a mouse message to the chord key it reports: `(vk, down)`. Returns
/// `None` for every message that is not a middle or X button press/release
/// (including `WM_MOUSEMOVE`), and for X buttons other than XBUTTON1/2.
fn mouse_button(message: u32, lparam: LPARAM) -> Option<(u32, bool)> {
    match message {
        WM_MBUTTONDOWN => Some((VK_MOUSE3, true)),
        WM_MBUTTONUP => Some((VK_MOUSE3, false)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            // SAFETY: for WH_MOUSE_LL with code >= 0, lParam points to an
            // MSLLHOOKSTRUCT that stays valid for the duration of this call.
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let button = (info.mouseData >> 16) & 0xFFFF;
            let vk = match button {
                XBUTTON1 => VK_MOUSE4,
                XBUTTON2 => VK_MOUSE5,
                _ => return None,
            };
            Some((vk, message == WM_XBUTTONDOWN))
        }
        _ => None,
    }
}

/// Low-level mouse hook procedure. Runs on the `hotkey-msg` thread inside the
/// message loop. It reacts only to middle and X button events and always calls
/// the next hook, so no click is swallowed.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        if let Some((vk, down)) = mouse_button(wparam.0 as u32, lparam) {
            with_ctx(|ctx| ctx.mouse(vk, down));
        }
    }
    let hook = CTX.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|guard| guard.as_ref().map(|ctx| ctx.mouse_hook))
    });
    // SAFETY: CallNextHookEx passes the event on. A null handle is accepted,
    // because the hook handle argument is ignored on current Windows versions.
    let hook = hook.unwrap_or(HHOOK(std::ptr::null_mut()));
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

/// Low-level keyboard hook procedure. Runs on the `hotkey-msg` thread inside
/// the message loop, so it must return quickly. It records the event and always
/// calls the next hook, so the key is never swallowed.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let message = wparam.0 as u32;
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if down || up {
            // SAFETY: for WH_KEYBOARD_LL with code >= 0, lParam points to a
            // KBDLLHOOKSTRUCT that stays valid for the duration of this call.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let (vk, flags) = (info.vkCode, info.flags.0);
            with_ctx(|ctx| ctx.key(vk, down, flags));
        }
    }
    let hook = CTX.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|guard| guard.as_ref().map(|ctx| ctx.hook))
    });
    // SAFETY: CallNextHookEx passes the event on. A null handle is accepted,
    // because the hook handle argument is ignored on current Windows versions.
    let hook = hook.unwrap_or(HHOOK(std::ptr::null_mut()));
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

/// Runs the GetMessage loop until WM_QUIT (or a queue error).
fn pump() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer; None receives all messages
        // for this thread.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            // 0 means WM_QUIT; -1 means the queue failed. Both end the loop.
            break;
        }
        match msg.message {
            WM_TIMER if msg.wParam.0 == GESTURE_TIMER => with_ctx(HookCtx::tick),
            WM_APP_APPLY => handle_apply(msg.lParam),
            _ => {
                // SAFETY: `msg` was just returned by GetMessageW.
                let _ = unsafe { DispatchMessageW(&msg) };
            }
        }
    }
}

/// Handles a `WM_APP_APPLY` message: swaps the bindings and replies.
/// A request whose caller already timed out is dropped without changes.
fn handle_apply(lparam: LPARAM) {
    // SAFETY: apply() created this pointer with Box::into_raw, posted it once,
    // and gave up ownership. This is the only place that reclaims it.
    let request = unsafe { Box::from_raw(lparam.0 as *mut ApplyRequest) };
    let ApplyRequest {
        registry,
        reply,
        gate,
    } = *request;

    if !gate.claim() {
        return;
    }

    with_ctx(|ctx| ctx.replace(&registry));
    let _ = reply.send(Vec::new());
}

/// Creates a message-only window (parent `HWND_MESSAGE`). Returns the Win32 code on failure.
fn create_message_window() -> Result<HWND, u32> {
    // SAFETY: "STATIC" is a predefined class, so no class registration is
    // needed. The string literals are static, and the parent is HWND_MESSAGE,
    // which makes the window message-only.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("clipcheck-hotkeys"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            None,
            None,
        )
    }
    .map_err(|e| win32_code(&e))
}

/// Destroys the message window. Errors are ignored during teardown.
fn destroy_window(hwnd: HWND) {
    // SAFETY: hwnd was created by this thread and is destroyed only once.
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// Extracts the Win32 error code from an HRESULT-wrapped `windows::core::Error`.
fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    }
}

#[cfg(test)]
mod apply_gate_tests {
    use super::{ApplyGate, ApplyState};

    #[test]
    fn claim_before_cancel_lets_request_run() {
        let gate = ApplyGate::new();
        assert!(gate.claim());
        assert!(!gate.cancel());
        assert_eq!(*gate.state(), ApplyState::Claimed);
    }

    #[test]
    fn cancel_before_claim_stops_request() {
        let gate = ApplyGate::new();
        assert!(gate.cancel());
        assert!(!gate.claim());
        assert_eq!(*gate.state(), ApplyState::Cancelled);
    }

    #[test]
    fn second_claim_or_cancel_is_refused() {
        let gate = ApplyGate::new();
        assert!(gate.claim());
        assert!(!gate.claim());
        let gate = ApplyGate::new();
        assert!(gate.cancel());
        assert!(!gate.cancel());
    }
}
