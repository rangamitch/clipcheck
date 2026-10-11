//! What the window's X button does, and how the UI reacts to daemon lifecycle events.
//!
//! The recorder daemon is a separate process that owns the tray icon. Closing the window
//! with `minimize_to_tray` ends only the UI process, so the daemon keeps recording.
//! `exit` also stops the daemon. [`decide_close`] is the pure decision table.
//!
//! Settings come from the daemon (`get_config`), cached in [`SharedCloseSettings`] and
//! refreshed on every connection and on every `config_changed` event.

use std::sync::{Arc, RwLock};
use std::thread;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::ipc::{request_shutdown, ClientError, DaemonClient, ShutdownRequest};

/// Tauri event sent to the frontend when an exit needs in-app confirmation.
pub const CONFIRM_EXIT_EVENT: &str = "confirm-exit";

/// `general.close_behavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloseBehavior {
    /// Hide to the tray: end the UI process only, the daemon keeps running.
    #[default]
    MinimizeToTray,
    /// Exit the application, stopping the daemon too.
    Exit,
}

/// What to do when the window close is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// End the UI process only. The daemon and tray keep running.
    ExitUiOnly,
    /// Ask the daemon to shut down, then end the UI process.
    StopDaemonAndExit,
    /// Show the in-app confirm dialog. The window stays open until the user confirms.
    AskConfirmThenExit,
}

/// What the UI knows about the daemon's recording session when a close is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// `get_status` reported a session.
    Active,
    /// `get_status` reported no session.
    Inactive,
    /// The link is down, or `get_status` failed, timed out or returned an unexpected
    /// reply. A recording may still be running.
    Unknown,
}

/// Decides the close action from the session state.
///
/// `minimize_to_tray` always ends only the UI. With `exit`, an unknown state always asks
/// (the daemon may be recording, and the confirm setting does not apply). An active
/// session asks when `confirm_enabled` is set. Otherwise the daemon is stopped.
pub fn decide_close_state(
    behavior: CloseBehavior,
    confirm_enabled: bool,
    state: SessionState,
) -> CloseAction {
    match behavior {
        CloseBehavior::MinimizeToTray => CloseAction::ExitUiOnly,
        CloseBehavior::Exit => match state {
            SessionState::Unknown => CloseAction::AskConfirmThenExit,
            SessionState::Active if confirm_enabled => CloseAction::AskConfirmThenExit,
            SessionState::Active | SessionState::Inactive => CloseAction::StopDaemonAndExit,
        },
    }
}

/// Boolean form of [`decide_close_state`]. A missing connection is an unknown state.
pub fn decide_close(
    behavior: CloseBehavior,
    confirm_enabled: bool,
    session_active: bool,
    daemon_connected: bool,
) -> CloseAction {
    let state = if !daemon_connected {
        SessionState::Unknown
    } else if session_active {
        SessionState::Active
    } else {
        SessionState::Inactive
    };
    decide_close_state(behavior, confirm_enabled, state)
}

/// The close-related settings read from the daemon's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseSettings {
    /// `general.close_behavior`. Unknown values fall back to `minimize_to_tray`.
    pub behavior: CloseBehavior,
    /// `general.confirm_exit_while_recording`. Defaults to true.
    pub confirm_enabled: bool,
}

impl Default for CloseSettings {
    fn default() -> Self {
        Self {
            behavior: CloseBehavior::MinimizeToTray,
            confirm_enabled: true,
        }
    }
}

impl CloseSettings {
    /// Reads the settings from a configuration object (the `config` field of a
    /// `get_config` reply). Missing or unknown values fall back to the defaults.
    pub fn from_config(config: &Value) -> Self {
        let general = config.get("general");
        let behavior = match general
            .and_then(|g| g.get("close_behavior"))
            .and_then(Value::as_str)
        {
            Some("exit") => CloseBehavior::Exit,
            _ => CloseBehavior::MinimizeToTray,
        };
        let confirm_enabled = general
            .and_then(|g| g.get("confirm_exit_while_recording"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Self {
            behavior,
            confirm_enabled,
        }
    }

    /// Reads the settings from a `get_config` reply body (`{"type":"config","data":{..}}`).
    /// Returns `None` when the body is not a config reply.
    pub fn from_config_reply(body: &Value) -> Option<Self> {
        if body.get("type").and_then(Value::as_str) != Some("config") {
            return None;
        }
        let config = body.get("data")?.get("config")?;
        Some(Self::from_config(config))
    }
}

/// The cached close settings, shared between the window handler and the event hook.
pub type SharedCloseSettings = Arc<RwLock<CloseSettings>>;

/// Creates the cache with the default settings.
pub fn new_cache() -> SharedCloseSettings {
    Arc::new(RwLock::new(CloseSettings::default()))
}

/// Returns a copy of the cached settings. A poisoned lock still yields its data.
pub fn read_cache(cache: &SharedCloseSettings) -> CloseSettings {
    match cache.read() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Fetches `get_config` from the daemon and stores the close settings in `cache`.
///
/// A reply that is not a config (for example a daemon error) leaves the cache as it was.
/// Returns the settings now in effect, or the request error.
pub fn refresh_settings(
    client: &DaemonClient,
    cache: &SharedCloseSettings,
) -> Result<CloseSettings, ClientError> {
    let body = client.request_command(json!({ "type": "get_config" }))?;
    if let Some(settings) = CloseSettings::from_config_reply(&body) {
        match cache.write() {
            Ok(mut guard) => *guard = settings,
            Err(poisoned) => *poisoned.into_inner() = settings,
        }
    }
    Ok(read_cache(cache))
}

/// Classifies a `get_status` reply body. Fails closed: the body must be an object with
/// `type == "status"` and an object `data` containing a `session` key. `session` JSON null
/// is [`SessionState::Inactive`], a JSON object is [`SessionState::Active`], and any other
/// shape is [`SessionState::Unknown`].
fn classify_status_body(body: &Value) -> SessionState {
    if body.get("type").and_then(Value::as_str) != Some("status") {
        return SessionState::Unknown;
    }
    let Some(data) = body.get("data").and_then(Value::as_object) else {
        return SessionState::Unknown;
    };
    match data.get("session") {
        Some(Value::Null) => SessionState::Inactive,
        Some(Value::Object(_)) => SessionState::Active,
        _ => SessionState::Unknown,
    }
}

/// True only when a `get_status` reply body reports an active recording session.
/// Anything else, including malformed bodies, counts as not active.
pub fn session_active_in_status(body: &Value) -> bool {
    classify_status_body(body) == SessionState::Active
}

/// Maps the outcome of a `get_status` request to a [`SessionState`]. A request error, a
/// daemon error reply or any malformed status body is [`SessionState::Unknown`].
pub fn session_state_from_reply(reply: Result<Value, ClientError>) -> SessionState {
    match reply {
        Ok(body) => classify_status_body(&body),
        Err(_) => SessionState::Unknown,
    }
}

/// Asks the daemon whether a recording session is active. Errors yield
/// [`SessionState::Unknown`].
pub fn session_state_now(client: &DaemonClient) -> SessionState {
    session_state_from_reply(client.request_command(json!({ "type": "get_status" })))
}

/// True only when the daemon reports an active session. Unknown counts as not active.
/// Used by the updater, which is not covered by the close-confirm logic.
pub fn session_active_now(client: &DaemonClient) -> bool {
    session_state_now(client) == SessionState::Active
}

/// True for the daemon's `app_exit` event (the daemon process is exiting for good).
pub fn is_app_exit_event(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("app_exit")
}

/// True for the daemon's `config_changed` event.
pub fn is_config_changed_event(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("config_changed")
}

/// Returns the managed daemon client, if it is already in Tauri state.
fn client_of(app: &AppHandle) -> Option<Arc<DaemonClient>> {
    app.try_state::<Arc<DaemonClient>>()
        .map(|state| Arc::clone(&*state))
}

/// Refreshes the cache from `client` on a background thread. The caller passes the client
/// it already holds, so no lookup or wait is needed. A failed request is logged, and the
/// cache keeps its previous values.
pub fn refresh_in_background(app: AppHandle, client: Arc<DaemonClient>) {
    thread::spawn(move || {
        let Some(cache) = app.try_state::<SharedCloseSettings>() else {
            eprintln!("clipcheck-ui: close settings cache not managed; close settings not loaded");
            return;
        };
        if let Err(e) = refresh_settings(&client, &cache) {
            eprintln!("clipcheck-ui: could not load close settings from the daemon: {e}");
        }
    });
}

/// Handles a daemon event for the close logic. Called from the daemon event hook.
///
/// `app_exit` ends the UI immediately, with no confirm. `config_changed` refreshes the
/// cache when `client` is available. `client` is `None` only before `setup` has published
/// the client; the startup refresh in `lib.rs` reads the current config then.
pub fn on_daemon_event(app: &AppHandle, event: &Value, client: Option<&Arc<DaemonClient>>) {
    if is_app_exit_event(event) {
        app.exit(0);
    } else if is_config_changed_event(event) {
        if let Some(client) = client {
            refresh_in_background(app.clone(), Arc::clone(client));
        }
    }
}

/// Shutdown mode for CloseAction::StopDaemonAndExit: Force when a session is active (the user
/// turned off the confirm), Graceful otherwise.
pub fn shutdown_mode_for(state: SessionState) -> ShutdownRequest {
    match state {
        SessionState::Active => ShutdownRequest::Force,
        SessionState::Inactive | SessionState::Unknown => ShutdownRequest::Graceful,
    }
}

/// True when a daemon reply is the `busy` refusal: `type == "error"` and `data.code == "busy"`.
pub fn is_busy_reply(reply: &Value) -> bool {
    reply.get("type").and_then(Value::as_str) == Some("error")
        && reply
            .get("data")
            .and_then(|d| d.get("code"))
            .and_then(Value::as_str)
            == Some("busy")
}

/// Runs the close decision for the main window. Called on a background thread, because
/// it makes daemon requests.
///
/// A `busy` reply to a graceful stop means a recording started after the status check. The
/// window then stays open and the confirm dialog is shown. Any other outcome exits the UI.
pub fn handle_close_request(app: &AppHandle) {
    let settings = app
        .try_state::<SharedCloseSettings>()
        .map(|cache| read_cache(&cache))
        .unwrap_or_default();
    let client = client_of(app);
    if client.is_none() {
        eprintln!(
            "clipcheck-ui: daemon client not managed; close decision uses unknown session state"
        );
    }
    let state = match &client {
        Some(c) if c.is_connected() => session_state_now(c),
        _ => SessionState::Unknown,
    };

    match decide_close_state(settings.behavior, settings.confirm_enabled, state) {
        CloseAction::ExitUiOnly => app.exit(0),
        CloseAction::StopDaemonAndExit => {
            if let Some(c) = &client {
                let mode = shutdown_mode_for(state);
                if let Ok(reply) = request_shutdown(c, mode) {
                    if is_busy_reply(&reply) {
                        // A recording started between the status check and this request.
                        let _ = app.emit(CONFIRM_EXIT_EVENT, ());
                        return;
                    }
                }
            }
            app.exit(0);
        }
        CloseAction::AskConfirmThenExit => {
            let _ = app.emit(CONFIRM_EXIT_EVENT, ());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_minimize_exits_ui_only() {
        for confirm in [true, false] {
            assert_eq!(
                decide_close(CloseBehavior::MinimizeToTray, confirm, true, false),
                CloseAction::ExitUiOnly
            );
        }
    }

    #[test]
    fn disconnected_exit_asks_and_never_exits_silently() {
        for confirm in [true, false] {
            assert_eq!(
                decide_close(CloseBehavior::Exit, confirm, false, false),
                CloseAction::AskConfirmThenExit
            );
        }
    }

    #[test]
    fn unknown_status_exit_asks_regardless_of_confirm() {
        for confirm in [true, false] {
            assert_eq!(
                decide_close_state(CloseBehavior::Exit, confirm, SessionState::Unknown),
                CloseAction::AskConfirmThenExit
            );
        }
        assert_eq!(
            decide_close_state(CloseBehavior::MinimizeToTray, true, SessionState::Unknown),
            CloseAction::ExitUiOnly
        );
    }

    #[test]
    fn known_status_exit_table() {
        use CloseAction::{AskConfirmThenExit, StopDaemonAndExit};
        assert_eq!(
            decide_close_state(CloseBehavior::Exit, true, SessionState::Active),
            AskConfirmThenExit
        );
        assert_eq!(
            decide_close_state(CloseBehavior::Exit, false, SessionState::Active),
            StopDaemonAndExit
        );
        assert_eq!(
            decide_close_state(CloseBehavior::Exit, true, SessionState::Inactive),
            StopDaemonAndExit
        );
    }

    #[test]
    fn failed_or_odd_status_reply_is_unknown() {
        assert_eq!(
            session_state_from_reply(Err(ClientError::Timeout)),
            SessionState::Unknown
        );
        assert_eq!(
            session_state_from_reply(Err(ClientError::Disconnected)),
            SessionState::Unknown
        );
        assert_eq!(
            session_state_from_reply(Ok(json!({ "type": "error" }))),
            SessionState::Unknown
        );
        assert_eq!(
            session_state_from_reply(Ok(json!({ "type": "status", "data": { "session": null } }))),
            SessionState::Inactive
        );
        assert_eq!(
            session_state_from_reply(Ok(
                json!({ "type": "status", "data": { "session": { "session_id": "s" } } })
            )),
            SessionState::Active
        );
    }

    #[test]
    fn default_settings_match_schema() {
        let d = CloseSettings::default();
        assert_eq!(d.behavior, CloseBehavior::MinimizeToTray);
        assert!(d.confirm_enabled);
    }

    #[test]
    fn unknown_close_behavior_falls_back() {
        let cfg = json!({ "general": { "close_behavior": "bogus" } });
        assert_eq!(
            CloseSettings::from_config(&cfg).behavior,
            CloseBehavior::MinimizeToTray
        );
    }

    #[test]
    fn status_without_session_is_inactive() {
        assert!(!session_active_in_status(
            &json!({ "type": "status", "data": { "session": null } })
        ));
        assert!(!session_active_in_status(&json!({ "type": "error" })));
        assert!(session_active_in_status(
            &json!({ "type": "status", "data": { "session": { "session_id": "s" } } })
        ));
    }

    #[test]
    fn shutdown_mode_follows_session_state() {
        assert_eq!(
            shutdown_mode_for(SessionState::Active),
            ShutdownRequest::Force
        );
        assert_eq!(
            shutdown_mode_for(SessionState::Inactive),
            ShutdownRequest::Graceful
        );
        assert_eq!(
            shutdown_mode_for(SessionState::Unknown),
            ShutdownRequest::Graceful
        );
    }

    #[test]
    fn only_busy_error_is_busy_reply() {
        assert!(is_busy_reply(
            &json!({ "type": "error", "data": { "code": "busy" } })
        ));
        assert!(!is_busy_reply(
            &json!({ "type": "error", "data": { "code": "internal" } })
        ));
        assert!(!is_busy_reply(&json!({ "type": "shutdown_ack" })));
        assert!(!is_busy_reply(&json!({})));
    }

    #[test]
    fn event_predicates_match_wire_names() {
        assert!(is_app_exit_event(&json!({ "type": "app_exit", "seq": 3 })));
        assert!(!is_app_exit_event(&json!({ "type": "shutting_down" })));
        assert!(is_config_changed_event(
            &json!({ "type": "config_changed" })
        ));
    }
}
