//! IPC accept loop and per-connection request handling.
//!
//! One thread accepts connections. Each accepted connection gets its own
//! thread. Every connection starts with a `hello` handshake, then serves
//! requests until the client disconnects.

use std::collections::VecDeque;
use std::io;
use std::process;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use clipcheck_core::config::validate::{Diagnostic, Severity};
use clipcheck_ipc::frame::{read_frame, write_frame, FrameError};
use clipcheck_ipc::proto::{
    negotiate, Command, DiagSeverity, DiagnosticDto, Envelope, ErrorCode, Reply, Request,
    ShutdownMode, Topic, PROTOCOL_MIN_SUPPORTED, PROTOCOL_VERSION,
};
use clipcheck_ipc::{PipeConn, PipeServer, MAX_CLIENTS};
use serde_json::Value;

use crate::app::{now_unix_ms, request_shutdown, AppState};
use crate::configstore::class_name;
use crate::discovery;
use crate::events::{Delivery, Subscription};
use crate::logging;
use crate::service::{ConfigOutcome, ServiceError};

// Lives in its own file, not in lib.rs (which another change edits): handlers for the recording
// management commands.
#[path = "recordings.rs"]
mod recordings;

/// Pause after a failed accept before retrying, so a broken pipe does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Accepts connections forever and serves each on its own thread.
///
/// Runs on the caller's thread because [`PipeServer`] is not `Send`. Shutdown
/// is handled by terminating the process, so this function never returns.
pub fn serve(mut pipe: PipeServer, state: Arc<AppState>) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let conn = match pipe.accept() {
            Ok(conn) => conn,
            Err(e) => {
                logging::warn(&format!("pipe accept failed: {e}"));
                thread::sleep(ACCEPT_RETRY);
                continue;
            }
        };
        if active.load(Ordering::SeqCst) >= usize::from(MAX_CLIENTS) {
            logging::warn("rejecting IPC client: too many connected clients");
            drop(conn);
            continue;
        }

        let slot = ClientSlot::new(Arc::clone(&active));
        let state = Arc::clone(&state);
        let spawned = thread::Builder::new()
            .name("ipc-client".to_owned())
            .spawn(move || {
                let _slot = slot;
                handle_connection(conn, &state);
            });
        if let Err(e) = spawned {
            // The closure was dropped, which released the slot and the connection.
            logging::warn(&format!("cannot spawn IPC client thread: {e}"));
        }
    }
}

/// Counts one connected client for as long as it is alive.
struct ClientSlot(Arc<AtomicUsize>);

impl ClientSlot {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The write side of one client connection. Replies and pushed events both
/// take this lock for each whole frame, so frames never interleave.
type Writer = Mutex<PipeConn>;

/// Runs the handshake and then the request loop for one client.
///
/// The connection is split in two: the reader (this thread) owns the original
/// [`PipeConn`] and blocks on requests, and a clone sits behind the [`Writer`]
/// lock. A dedicated event thread blocks on the bus subscription and writes each
/// pushed event as soon as it is published, even while the client is idle.
/// Dropping the subscription at teardown closes the event channel, which stops
/// that thread.
fn handle_connection(mut conn: PipeConn, state: &AppState) {
    let writer: Arc<Writer> = match conn.try_clone() {
        Ok(clone) => Arc::new(Mutex::new(clone)),
        Err(e) => {
            logging::warn(&format!("cannot clone IPC connection: {e}"));
            return;
        }
    };

    let proto = match handshake(&mut conn, &writer) {
        Ok(proto) => proto,
        Err(e) => {
            logging::debug(&format!("IPC handshake rejected: {e}"));
            return;
        }
    };

    let (subscription, events) = state.bus.subscribe();
    if let Err(e) = spawn_event_writer(Arc::clone(&writer), proto, events) {
        logging::warn(&format!("cannot spawn IPC event thread: {e}"));
        return;
    }

    let mut subscriptions: Vec<Topic> = Vec::new();
    let mut replays = ReplayCache::default();
    let owner = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    // Whether this client holds a hotkey suspension. Its disconnect ends the suspension.
    let mut suspending_hotkeys = false;
    loop {
        let frame: Value = match read_frame(&mut conn) {
            Ok(frame) => frame,
            Err(FrameError::Eof) => break,
            Err(FrameError::TooLarge(_)) => {
                let _ = send_error(
                    &writer,
                    proto,
                    None,
                    ErrorCode::FrameTooLarge,
                    "frame exceeds the size limit",
                );
                break;
            }
            Err(FrameError::Json(e)) => {
                if send_error(
                    &writer,
                    proto,
                    None,
                    ErrorCode::BadRequest,
                    &format!("invalid json: {e}"),
                )
                .is_err()
                {
                    break;
                }
                continue;
            }
            Err(e) => {
                logging::debug(&format!("IPC connection ended: {e}"));
                break;
            }
        };

        let id = frame.get("id").and_then(Value::as_u64);
        let request: Request = match serde_json::from_value(frame.clone()) {
            Ok(request) => request,
            Err(e) => {
                let code = unparsed_request_code(&frame);
                if send_error(&writer, proto, id, code, &e.to_string()).is_err() {
                    break;
                }
                continue;
            }
        };

        if request.v != proto {
            let message = format!(
                "frame version {} does not match negotiated protocol {proto}",
                request.v
            );
            if send_error(
                &writer,
                proto,
                request.id,
                ErrorCode::ProtoMismatch,
                &message,
            )
            .is_err()
            {
                break;
            }
            continue;
        }

        let reply = replay_or_run(&mut replays, request.id, request.body, |body| {
            dispatch(
                body,
                state,
                &subscription,
                &mut subscriptions,
                owner,
                &mut suspending_hotkeys,
            )
        });
        // Only an accepted shutdown (ShutdownAck) stops the daemon; a busy refusal keeps it running.
        let shutdown_requested = matches!(reply, Reply::ShutdownAck);
        // The ack must be written before the watcher thread exits the process.
        let wrote = send(&writer, proto, request.id, reply);
        if shutdown_requested {
            logging::info("shutdown requested by IPC client");
            request_shutdown();
            break;
        }
        if wrote.is_err() {
            break;
        }
    }

    // Unregistering closes the event channel, so the event thread exits after
    // its current write. Its clone of the connection is released then.
    discovery::meter_release(owner);
    // A client that goes away mid-capture must not leave the hotkeys suspended.
    if suspending_hotkeys {
        if let Err(e) = state.service.set_hotkeys_suspended(false) {
            logging::warn(&format!(
                "could not resume hotkeys after IPC client left: {}",
                e.message
            ));
        }
    }
    drop(subscription);
    logging::debug("IPC client disconnected");
}

/// Starts the thread that writes bus events for one client as they arrive.
///
/// The thread exits when the subscription's sender is dropped, or when a write
/// fails because the client is gone.
fn spawn_event_writer(
    writer: Arc<Writer>,
    proto: u16,
    events: Receiver<Delivery>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("ipc-events".to_owned())
        .spawn(move || {
            while let Ok(delivery) = events.recv() {
                // Serializes the shared event in place; no per-subscriber copy.
                let frame = Envelope {
                    v: proto,
                    id: None,
                    seq: Some(delivery.seq),
                    body: &*delivery.event,
                };
                let mut conn = lock(&writer);
                if write_frame(&mut *conn, &frame).is_err() {
                    break;
                }
            }
        })
        .map(|_| ())
}

/// Locks a client's writer. A poisoned lock still guards a valid connection,
/// so the inner value is used.
fn lock(writer: &Writer) -> MutexGuard<'_, PipeConn> {
    writer.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads the first frame, which must be `hello`, and answers `hello_ack`.
/// Returns the negotiated protocol version.
fn handshake(conn: &mut PipeConn, writer: &Writer) -> Result<u16, String> {
    let frame: Value = read_frame(conn).map_err(|e| e.to_string())?;
    let id = frame.get("id").and_then(Value::as_u64);
    let request: Request = match serde_json::from_value(frame.clone()) {
        Ok(request) => request,
        Err(e) => {
            let _ = send_error(
                writer,
                PROTOCOL_VERSION,
                id,
                unparsed_request_code(&frame),
                &e.to_string(),
            );
            return Err(format!("malformed hello: {e}"));
        }
    };

    let Command::Hello {
        proto_min,
        proto_max,
        client,
        client_version,
    } = request.body
    else {
        let _ = send_error(
            writer,
            PROTOCOL_VERSION,
            id,
            ErrorCode::Unauthorized,
            "the first frame must be hello",
        );
        return Err("first frame was not hello".to_owned());
    };

    if request.v != PROTOCOL_VERSION {
        let _ = send_error(
            writer,
            PROTOCOL_VERSION,
            id,
            ErrorCode::ProtoMismatch,
            &format!("hello must use frame version {PROTOCOL_VERSION}"),
        );
        return Err(format!("hello used frame version {}", request.v));
    }

    let Some(proto) = negotiate(
        proto_min,
        proto_max,
        PROTOCOL_MIN_SUPPORTED,
        PROTOCOL_VERSION,
    ) else {
        let _ = send_error(
            writer,
            PROTOCOL_VERSION,
            id,
            ErrorCode::ProtoMismatch,
            "no protocol version in common",
        );
        return Err(format!(
            "client {client} supports no common protocol version"
        ));
    };

    let ack = Reply::HelloAck {
        proto,
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        daemon_pid: process::id(),
        features: Vec::new(),
    };
    send(writer, proto, id, ack).map_err(|e| e.to_string())?;
    logging::info(&format!(
        "IPC client {client} {client_version} connected, protocol {proto}"
    ));
    Ok(proto)
}

/// Identifies each IPC connection, so a meter can be stopped when its client leaves.
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// Wire names (`body.type`) of every [`Command`] variant. Must list the same names as the
/// serde tags of `CommandWire` in `clipcheck_ipc::proto`. A new variant that is missing here
/// is reported as `unknown_type`, so add it together with the variant.
const COMMAND_TYPES: &[&str] = &[
    "hello",
    "ping",
    "get_status",
    "start_session",
    "stop_session",
    "split_session",
    "clip_now",
    "get_config",
    "patch_config",
    "reload_config",
    "rename_recording",
    "set_favorite",
    "delete_recording",
    "register_edited_recording",
    "subscribe",
    "unsubscribe",
    "shutdown",
    "list_audio_apps",
    "list_audio_devices",
    "list_encoders",
    "list_running_apps",
    "mic_meter",
    "mic_test",
    "list_libraries",
    "scan_libraries",
    "import_plan",
    "import_start",
    "import_cancel",
    "list_game_names",
];

/// Chooses the error code for a frame that did not parse as a [`Request`].
///
/// The decision uses the frame's `body.type` string, checked against [`COMMAND_TYPES`], and
/// not the text of the serde error, which may change between library versions. A frame whose
/// type is missing or not a string is `bad_request`.
fn unparsed_request_code(frame: &Value) -> ErrorCode {
    match frame
        .get("body")
        .and_then(|body| body.get("type"))
        .and_then(Value::as_str)
    {
        Some(name) if !COMMAND_TYPES.contains(&name) => ErrorCode::UnknownType,
        _ => ErrorCode::BadRequest,
    }
}

/// Produces the reply for one command. `owner` identifies the connection, and
/// `suspending_hotkeys` records whether that connection holds a hotkey suspension.
fn dispatch(
    command: Command,
    state: &AppState,
    subscription: &Subscription,
    subscriptions: &mut Vec<Topic>,
    owner: u64,
    suspending_hotkeys: &mut bool,
) -> Reply {
    match command {
        Command::Ping => Reply::Pong {
            unix_ms: now_unix_ms(),
        },
        Command::GetStatus => match state.status() {
            Ok(status) => Reply::Status(Box::new(status)),
            Err(e) => service_error(e),
        },
        Command::StartSession => ack(state.service.start_session()),
        Command::StopSession => ack(state.service.stop_session()),
        Command::SplitSession => ack(state.service.split_session()),
        Command::SetRecorderEnabled { enabled } => ack(state.service.set_recorder_enabled(enabled)),
        Command::SetHotkeysSuspended { suspended } => {
            let reply = ack(state.service.set_hotkeys_suspended(suspended));
            if matches!(reply, Reply::Ok) {
                *suspending_hotkeys = suspended;
            }
            reply
        }
        // The length is validated once, in `Service::clip`, which the hotkey path also uses.
        Command::ClipNow { seconds } => match state.service.clip(seconds) {
            Ok(clip_id) => Reply::ClipQueued { clip_id },
            Err(e) => service_error(e),
        },
        Command::GetConfig => match state.service.config() {
            Ok(view) => Reply::Config {
                config: view.config,
                config_version: view.version,
                path: view.path,
            },
            Err(e) => service_error(e),
        },
        Command::PatchConfig { patch, dry_run } => {
            config_reply(state.service.patch_config(patch, dry_run))
        }
        Command::ReloadConfig => config_reply(state.service.reload_config()),
        Command::Subscribe { topics } => {
            discovery::ensure_audio_poller();
            for topic in &topics {
                if !subscriptions.contains(topic) {
                    subscriptions.push(*topic);
                }
            }
            subscription.set_topics(subscriptions);
            Reply::Subscribed { topics }
        }
        Command::Unsubscribe { topics } => {
            subscriptions.retain(|topic| !topics.contains(topic));
            subscription.set_topics(subscriptions);
            Reply::Unsubscribed { topics }
        }
        Command::Hello { .. } => Reply::Error {
            code: ErrorCode::BadRequest,
            message: "hello was already completed on this connection".to_owned(),
            retryable: false,
        },
        // The connection loop triggers the actual shutdown after this ack is written.
        Command::Shutdown { reason, mode } => shutdown_reply(state, reason.as_deref(), mode),
        Command::ListAudioApps => discovery::audio_apps_reply(),
        Command::ListAudioDevices => discovery::audio_devices_reply(),
        Command::ListEncoders => match state.service.encoder_report() {
            Ok(report) => discovery::encoders_reply_for(report.as_ref()),
            Err(e) => service_error(e),
        },
        Command::ListRunningApps => {
            let config = match state.service.config() {
                Ok(view) => view.config,
                Err(e) => return service_error(e),
            };
            let attached = state
                .service
                .status()
                .ok()
                .and_then(|status| status.game.map(|game| game.exe));
            discovery::running_apps_reply(&config, attached.as_deref())
        }
        Command::MicMeter { enable, device } => {
            match discovery::mic_meter(owner, enable, device, Arc::clone(&state.bus)) {
                Ok(()) => Reply::Ok,
                Err(message) => Reply::Error {
                    code: ErrorCode::WorkerUnavailable,
                    message,
                    retryable: true,
                },
            }
        }
        Command::MicTest { device } => discovery::mic_test_reply(device),
        Command::RenameRecording { recording_id, name } => {
            recordings::rename(state, &recording_id, &name)
        }
        Command::SetFavorite {
            recording_id,
            favorite,
        } => recordings::set_favorite(state, &recording_id, favorite),
        Command::DeleteRecording { recording_id } => recordings::delete(state, &recording_id),
        Command::RegisterEditedRecording {
            source_recording_id,
            path,
            keep_source_time,
        } => recordings::register_edited(
            state,
            source_recording_id.as_deref(),
            &path,
            keep_source_time,
        ),
        command @ (Command::ListLibraries
        | Command::ScanLibraries { .. }
        | Command::ImportPlan { .. }
        | Command::ImportStart { .. }
        | Command::ImportCancel { .. }
        | Command::ListGameNames) => state.service.library().handle(command),
    }
}

/// Replies remembered per connection for retried requests.
const REPLAY_WINDOW: usize = 256;

/// Whether running `command` twice would have an effect a retry must not repeat.
fn is_replayable(command: &Command) -> bool {
    matches!(
        command,
        Command::StartSession
            | Command::StopSession
            | Command::SplitSession
            | Command::ClipNow { .. }
            | Command::PatchConfig { .. }
            | Command::ReloadConfig
    )
}

/// The last [`REPLAY_WINDOW`] replies on one connection, keyed by request id.
///
/// Ids are only meaningful within one connection, so each connection owns its
/// cache. An entry matches only when both the id and the full command are equal,
/// so a reused id carrying a different command runs as a new request.
#[derive(Default)]
struct ReplayCache {
    entries: VecDeque<(u64, Command, Reply)>,
}

impl ReplayCache {
    fn lookup(&self, id: u64, command: &Command) -> Option<Reply> {
        self.entries
            .iter()
            .find(|(seen, body, _)| *seen == id && body == command)
            .map(|(_, _, reply)| reply.clone())
    }

    fn remember(&mut self, id: u64, command: Command, reply: Reply) {
        if self.entries.len() == REPLAY_WINDOW {
            self.entries.pop_front();
        }
        self.entries.push_back((id, command, reply));
    }
}

/// Answers a request. A repeated id for a replayable command returns the reply
/// remembered from its first run, so `run` (the handler) executes once.
/// Requests without an id, or for other commands, always run.
fn replay_or_run(
    cache: &mut ReplayCache,
    id: Option<u64>,
    command: Command,
    run: impl FnOnce(Command) -> Reply,
) -> Reply {
    let Some(id) = id.filter(|_| is_replayable(&command)) else {
        return run(command);
    };
    if let Some(reply) = cache.lookup(id, &command) {
        return reply;
    }
    let remembered = command.clone();
    let reply = run(command);
    // A reply that says "retry later" (for example Busy) must not be replayed: a retry with the
    // same id after the condition cleared has to run again.
    if !matches!(
        reply,
        Reply::Error {
            retryable: true,
            ..
        }
    ) {
        cache.remember(id, remembered, reply.clone());
    }
    reply
}

/// Maps a patch or reload outcome to its reply.
fn config_reply(result: Result<ConfigOutcome, ServiceError>) -> Reply {
    match result {
        Ok(ConfigOutcome::Applied {
            class,
            restart_required,
            warnings,
        }) => Reply::ConfigApplied {
            apply_class: class_name(class).to_owned(),
            restart_required,
            diagnostics: warnings.iter().map(diagnostic_dto).collect(),
        },
        Ok(ConfigOutcome::Checked {
            class,
            restart_required,
            warnings,
        }) => Reply::ConfigChecked {
            apply_class: class_name(class).to_owned(),
            restart_required,
            diagnostics: warnings.iter().map(diagnostic_dto).collect(),
        },
        Ok(ConfigOutcome::Rejected(findings)) => Reply::ConfigRejected {
            diagnostics: findings.iter().map(diagnostic_dto).collect(),
        },
        Err(e) => service_error(e),
    }
}

/// Converts a validation finding to its wire form.
fn diagnostic_dto(d: &Diagnostic) -> DiagnosticDto {
    DiagnosticDto {
        code: d.code.to_owned(),
        severity: match d.severity {
            Severity::Error => DiagSeverity::Error,
            Severity::Warning => DiagSeverity::Warning,
        },
        path: d.path.clone(),
        message: d.message.clone(),
    }
}

/// Decides the reply to `shutdown` (D47). The mode is `ShutdownMode::effective(mode,
/// reason)`. `graceful` and `update` are refused with `busy` while a recording session is
/// active (the replay buffer alone does not count); the caller then keeps running.
/// `force` always acknowledges. The check and the shutdown mark are one atomic step in
/// the service, so no session can start after an accepted request.
fn shutdown_reply(state: &AppState, reason: Option<&str>, mode: Option<ShutdownMode>) -> Reply {
    let mode = ShutdownMode::effective(mode, reason);
    logging::info(&format!(
        "shutdown command received (reason: {}, mode: {})",
        reason.unwrap_or("none"),
        mode_name(mode)
    ));
    match state.service.begin_shutdown(mode == ShutdownMode::Force) {
        Ok(()) => Reply::ShutdownAck,
        Err(e) if e.code == ErrorCode::Busy => {
            logging::info(&format!(
                "shutdown ({}) refused: a recording session is in progress",
                mode_name(mode)
            ));
            Reply::Error {
                code: ErrorCode::Busy,
                message: "A recording session is in progress".to_owned(),
                retryable: true,
            }
        }
        Err(e) => {
            logging::warn(&format!(
                "shutdown ({}) accepted although the service reported: {}",
                mode_name(mode),
                e.message
            ));
            Reply::ShutdownAck
        }
    }
}

fn mode_name(mode: ShutdownMode) -> &'static str {
    match mode {
        ShutdownMode::Graceful => "graceful",
        ShutdownMode::Update => "update",
        ShutdownMode::Force => "force",
    }
}

/// Maps a service failure to an error reply. Busy is the only retryable code.
fn service_error(e: ServiceError) -> Reply {
    let retryable = e.code == ErrorCode::Busy;
    Reply::Error {
        code: e.code,
        message: e.message,
        retryable,
    }
}

/// Reply for a command that only succeeds or fails.
fn ack(result: Result<(), ServiceError>) -> Reply {
    match result {
        Ok(()) => Reply::Ok,
        Err(e) => service_error(e),
    }
}

/// Writes one reply frame under the writer lock.
fn send(writer: &Writer, proto: u16, id: Option<u64>, body: Reply) -> Result<(), FrameError> {
    let mut conn = lock(writer);
    write_frame(
        &mut *conn,
        &Envelope {
            v: proto,
            id,
            seq: None,
            body,
        },
    )
}

/// Writes an error reply under the writer lock.
fn send_error(
    writer: &Writer,
    proto: u16,
    id: Option<u64>,
    code: ErrorCode,
    message: &str,
) -> Result<(), FrameError> {
    send(
        writer,
        proto,
        id,
        Reply::Error {
            code,
            message: message.to_owned(),
            retryable: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn clip() -> Command {
        Command::ClipNow { seconds: 5 }
    }

    fn queued(n: usize) -> Reply {
        Reply::ClipQueued {
            clip_id: format!("clip-{n}"),
        }
    }

    #[test]
    fn repeated_id_runs_handler_once_and_returns_same_reply() {
        let mut cache = ReplayCache::default();
        let runs = Cell::new(0usize);
        let run = |_: Command| {
            runs.set(runs.get() + 1);
            queued(runs.get())
        };
        let first = replay_or_run(&mut cache, Some(7), clip(), run);
        let second = replay_or_run(&mut cache, Some(7), clip(), run);
        assert_eq!(runs.get(), 1);
        assert_eq!(first, second);
    }

    #[test]
    fn retryable_error_replies_are_not_replayed() {
        let mut cache = ReplayCache::default();
        let runs = Cell::new(0usize);
        let run = |_: Command| {
            runs.set(runs.get() + 1);
            if runs.get() == 1 {
                Reply::Error {
                    code: clipcheck_ipc::proto::ErrorCode::Busy,
                    message: "busy".into(),
                    retryable: true,
                }
            } else {
                queued(runs.get())
            }
        };
        let first = replay_or_run(&mut cache, Some(9), clip(), run);
        assert!(matches!(
            first,
            Reply::Error {
                retryable: true,
                ..
            }
        ));
        let second = replay_or_run(&mut cache, Some(9), clip(), run);
        assert_eq!(
            runs.get(),
            2,
            "the retry after a retryable error must run again"
        );
        assert!(matches!(second, Reply::ClipQueued { .. }));
        let third = replay_or_run(&mut cache, Some(9), clip(), run);
        assert_eq!(runs.get(), 2, "a successful reply is replayed");
        assert_eq!(second, third);
    }

    #[test]
    fn reused_id_with_different_command_runs_again() {
        let mut cache = ReplayCache::default();
        let runs = Cell::new(0usize);
        let run = |_: Command| {
            runs.set(runs.get() + 1);
            queued(runs.get())
        };
        replay_or_run(&mut cache, Some(7), clip(), run);
        replay_or_run(&mut cache, Some(7), Command::ReloadConfig, run);
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn non_mutating_and_id_less_requests_always_run() {
        let mut cache = ReplayCache::default();
        let runs = Cell::new(0usize);
        let run = |_: Command| {
            runs.set(runs.get() + 1);
            Reply::Ok
        };
        replay_or_run(&mut cache, Some(1), Command::GetStatus, run);
        replay_or_run(&mut cache, Some(1), Command::GetStatus, run);
        replay_or_run(&mut cache, None, clip(), run);
        replay_or_run(&mut cache, None, clip(), run);
        assert_eq!(runs.get(), 4);
    }

    #[test]
    fn oldest_entry_is_evicted_after_the_window() {
        let mut cache = ReplayCache::default();
        let runs = Cell::new(0usize);
        let run = |_: Command| {
            runs.set(runs.get() + 1);
            Reply::Ok
        };
        let total = REPLAY_WINDOW as u64 + 1;
        for id in 1..=total {
            replay_or_run(&mut cache, Some(id), clip(), run);
        }
        assert_eq!(runs.get(), REPLAY_WINDOW + 1);
        // The newest id is still remembered.
        replay_or_run(&mut cache, Some(total), clip(), run);
        assert_eq!(runs.get(), REPLAY_WINDOW + 1);
        // Id 1 fell out of the window and runs again.
        replay_or_run(&mut cache, Some(1), clip(), run);
        assert_eq!(runs.get(), REPLAY_WINDOW + 2);
    }
}
