//! Connection from the UI to the recorder daemon over the IPC pipe (protocol v1).
//!
//! [`DaemonClient`] has no Tauri dependency. A manager thread connects, performs the
//! Hello handshake, subscribes to every topic and reconnects with backoff. Each
//! connection is a [`Link`] with its own reader thread, which routes replies to the
//! waiting request and pushes events to a callback. The Tauri commands at the bottom
//! of this file are thin wrappers around [`DaemonClient`].
//!
//! Every [`Link`] is closed when it ends, including one whose handshake failed. Closing
//! cancels the overlapped read, so the reader thread exits even while the daemon is
//! unresponsive (see [`PipeConn::cancel_pending`]).

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_ipc::frame::{read_frame, write_frame, FrameError};
use clipcheck_ipc::proto::{
    Command, Envelope, Reply, Topic, PROTOCOL_MIN_SUPPORTED, PROTOCOL_VERSION,
};
use clipcheck_ipc::{PipeClient, PipeConn, HANDSHAKE_TIMEOUT};
use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::daemon::resolve_pipe_name;

/// Client name sent in the Hello handshake.
const CLIENT_NAME: &str = "clipcheck-ui";
/// Time allowed for one connect attempt before backing off.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Default request timeout. The daemon serves one connection serially, so a request can
/// queue behind slow disk, ffprobe or SQLite work; 5 s was too tight for that.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for requests that write configuration, run the mic test, or do library and
/// recording file operations (ffprobe, thumbnails, Recycle Bin, scans, imports).
const LONG_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Time to wait for the shutdown ack before the UI exits anyway.
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// First reconnect delay. Doubles after each failed attempt.
const BACKOFF_START: Duration = Duration::from_millis(250);
/// Upper bound for the reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(2);
/// Interval at which blocking waits re-check the stop flag.
const POLL: Duration = Duration::from_millis(100);
/// Topics subscribed after each successful handshake. `levels` is left out on purpose: the
/// daemon sends it about 10 Hz, and no view in the UI consumes it (the frontend's
/// `STATUS_TOPICS` omits it too). See [`LEVELS_TOPICS`] to enable it again.
const DEFAULT_TOPICS: [Topic; 7] = [
    Topic::State,
    Topic::Clips,
    Topic::Sessions,
    Topic::Health,
    Topic::Warnings,
    Topic::Settings,
    Topic::Library,
];
/// Topics that are not subscribed by default. Add these to the subscription (in
/// `open_session`) when a view needs audio levels.
#[allow(dead_code)]
const LEVELS_TOPICS: [Topic; 1] = [Topic::Levels];

/// Failures from [`DaemonClient::request`]. A daemon `Reply::Error` is not a failure;
/// it is returned as a normal reply body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No live connection, or the connection dropped before the reply arrived.
    Disconnected,
    /// No reply within the timeout. The daemon may still process the request.
    Timeout,
    /// The command JSON is not a valid `proto::Command`, or cannot be framed.
    InvalidCommand(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Disconnected => write!(f, "daemon disconnected"),
            ClientError::Timeout => write!(f, "daemon did not reply in time"),
            ClientError::InvalidCommand(msg) => write!(f, "invalid daemon command: {msg}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Returns the request timeout for a command JSON body: 30 s for configuration writes,
/// the mic test, and library/recording file operations; 15 s otherwise.
pub fn timeout_for(command: &Value) -> Duration {
    match command.get("type").and_then(Value::as_str) {
        Some("patch_config")
        | Some("reload_config")
        | Some("mic_test")
        | Some("register_edited_recording")
        | Some("rename_recording")
        | Some("delete_recording")
        | Some("set_favorite")
        | Some("scan_libraries")
        | Some("import_plan")
        | Some("import_start")
        | Some("import_cancel")
        | Some("list_libraries")
        | Some("list_game_names")
        | Some("clip_now") => LONG_REQUEST_TIMEOUT,
        _ => REQUEST_TIMEOUT,
    }
}

type Waiter = mpsc::Sender<Result<Value, ClientError>>;

/// Requests in flight on one connection. `open` turns false once the connection is
/// closed, after which no new request may register.
struct Pending {
    open: bool,
    waiters: HashMap<u64, Waiter>,
}

/// One connection to the daemon. Reads happen on the reader thread through a clone of
/// the pipe, and writes go through `writer`. The pipe is overlapped, so the two run
/// concurrently.
struct Link {
    pending: Mutex<Pending>,
    writer: Mutex<PipeConn>,
    /// Clone of the same pipe, used only to cancel the reader's pending read on close.
    canceller: PipeConn,
}

impl Link {
    fn new(writer: PipeConn, canceller: PipeConn) -> Self {
        Self {
            pending: Mutex::new(Pending {
                open: true,
                waiters: HashMap::new(),
            }),
            writer: Mutex::new(writer),
            canceller,
        }
    }

    /// Sends one command under `id` and waits up to `timeout` for the reply body.
    fn send(&self, id: u64, command: Command, timeout: Duration) -> Result<Value, ClientError> {
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = lock(&self.pending);
            if !pending.open {
                return Err(ClientError::Disconnected);
            }
            pending.waiters.insert(id, tx);
        }

        let written = {
            let mut writer = lock(&self.writer);
            write_frame(&mut *writer, &Envelope::request(id, command))
        };
        if let Err(e) = written {
            lock(&self.pending).waiters.remove(&id);
            return Err(match e {
                FrameError::TooLarge(n) => {
                    ClientError::InvalidCommand(format!("frame of {n} bytes is too large"))
                }
                FrameError::Json(e) => ClientError::InvalidCommand(e.to_string()),
                _ => ClientError::Disconnected,
            });
        }

        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
            Err(RecvTimeoutError::Timeout) => {
                let still_waiting = lock(&self.pending).waiters.remove(&id).is_some();
                if still_waiting {
                    Err(ClientError::Timeout)
                } else {
                    // The reader took the waiter just before the timeout and is sending
                    // the reply now, so take it instead of reporting a timeout.
                    match rx.recv() {
                        Ok(result) => result,
                        Err(_) => Err(ClientError::Disconnected),
                    }
                }
            }
        }
    }

    /// Hands a reply body to the request waiting on `id`, if one is still waiting.
    fn deliver(&self, id: u64, body: Value) {
        let waiter = lock(&self.pending).waiters.remove(&id);
        if let Some(tx) = waiter {
            let _ = tx.send(Ok(body));
        }
    }

    /// Marks the connection closed, fails every request still waiting on it, and cancels
    /// the reader's pending read so the reader thread can exit.
    fn close(&self) {
        let waiters = {
            let mut pending = lock(&self.pending);
            pending.open = false;
            std::mem::take(&mut pending.waiters)
        };
        for (_, tx) in waiters {
            let _ = tx.send(Err(ClientError::Disconnected));
        }
        if let Err(e) = self.canceller.cancel_pending() {
            eprintln!("clipcheck-ui: cannot cancel a pending daemon read: {e}");
        }
    }
}

/// Callbacks supplied by the owner of the client.
struct Callbacks {
    on_event: Box<dyn Fn(Value) + Send + Sync>,
    on_connection: Box<dyn Fn(bool) + Send + Sync>,
}

/// State shared by the manager thread, reader threads and request callers.
struct Inner {
    next_id: AtomicU64,
    link: Mutex<Option<Arc<Link>>>,
    connected: AtomicBool,
    stop: AtomicBool,
    callbacks: Arc<Callbacks>,
}

impl Inner {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Client for the recorder daemon. Connects in the background and reconnects on its own.
pub struct DaemonClient {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
    /// Why no connection manager runs, when startup failed. `None` when it started.
    start_error: Option<String>,
}

impl DaemonClient {
    /// Starts the connection manager.
    ///
    /// `pipe_name` wins when set. Otherwise the `CLIPCHECK_PIPE_NAME` environment
    /// variable is used, then the per-user default pipe (see
    /// [`resolve_pipe_name`]). `on_event` receives each pushed
    /// `Event` as a JSON object with its `seq` added. `on_connection` is called with
    /// `true` after a handshake and subscribe succeed, and with `false` when that
    /// connection is lost.
    ///
    /// If the pipe name cannot be resolved, the failure is logged, stored, and returned by
    /// [`DaemonClient::connection_error`]; the returned client never connects. Use
    /// [`DaemonClient::try_start`] to get the error as a `Result` instead.
    pub fn start(
        pipe_name: Option<String>,
        on_event: Box<dyn Fn(Value) + Send + Sync>,
        on_connection: Box<dyn Fn(bool) + Send + Sync>,
    ) -> DaemonClient {
        match resolve_pipe_name(pipe_name.as_deref()) {
            Ok(name) => Self::with_name(name, on_event, on_connection),
            Err(e) => {
                let msg = e.to_string();
                eprintln!("clipcheck-ui: daemon connection disabled: {msg}");
                DaemonClient {
                    inner: new_inner(on_event, on_connection),
                    thread: None,
                    start_error: Some(msg),
                }
            }
        }
    }

    /// The reason the connection manager did not start (pipe name unresolvable, or the
    /// thread could not be spawned). `None` when it started. A `Some` value means
    /// [`DaemonClient::is_connected`] will stay `false` for the whole session.
    pub fn connection_error(&self) -> Option<&str> {
        self.start_error.as_deref()
    }

    /// Same as [`DaemonClient::start`], but returns the pipe name resolution error to the
    /// caller instead of logging it and running without a connection.
    pub fn try_start(
        pipe_name: Option<String>,
        on_event: Box<dyn Fn(Value) + Send + Sync>,
        on_connection: Box<dyn Fn(bool) + Send + Sync>,
    ) -> io::Result<DaemonClient> {
        let name = resolve_pipe_name(pipe_name.as_deref())?;
        Ok(Self::with_name(name, on_event, on_connection))
    }

    /// Starts the manager thread for an already resolved pipe name.
    fn with_name(
        name: String,
        on_event: Box<dyn Fn(Value) + Send + Sync>,
        on_connection: Box<dyn Fn(bool) + Send + Sync>,
    ) -> DaemonClient {
        let inner = new_inner(on_event, on_connection);
        let thread_inner = Arc::clone(&inner);
        let mut start_error = None;
        let thread = match thread::Builder::new()
            .name("daemon-ipc-manager".into())
            .spawn(move || manage(thread_inner, name))
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                let msg = format!("cannot start the daemon connection thread: {e}");
                eprintln!("clipcheck-ui: {msg}");
                start_error = Some(msg);
                None
            }
        };
        DaemonClient {
            inner,
            thread,
            start_error,
        }
    }

    /// Sends a command with the timeout for its type (see [`timeout_for`]).
    pub fn request_command(&self, cmd: Value) -> Result<Value, ClientError> {
        let timeout = timeout_for(&cmd);
        self.request(cmd, timeout)
    }

    /// Sends `cmd` (a JSON `proto::Command`) and returns the reply body as JSON.
    ///
    /// A `Reply::Error` comes back as `Ok` with that body. Errors are returned only for a
    /// missing connection, a timeout, or a command that is not a valid `Command`.
    pub fn request(&self, cmd: Value, timeout: Duration) -> Result<Value, ClientError> {
        let command: Command =
            serde_json::from_value(cmd).map_err(|e| ClientError::InvalidCommand(e.to_string()))?;
        let link = lock(&self.inner.link)
            .clone()
            .ok_or(ClientError::Disconnected)?;
        link.send(self.inner.next_id(), command, timeout)
    }

    /// True while a handshaked, subscribed connection is up.
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Stops the manager and waits for it to finish. Reader threads are not joined; they
    /// end once their pipe read is cancelled or the daemon closes the pipe.
    pub fn stop(mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DaemonClient {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
    }
}

/// Creates the shared state with no connection yet.
fn new_inner(
    on_event: Box<dyn Fn(Value) + Send + Sync>,
    on_connection: Box<dyn Fn(bool) + Send + Sync>,
) -> Arc<Inner> {
    Arc::new(Inner {
        next_id: AtomicU64::new(1),
        link: Mutex::new(None),
        connected: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        callbacks: Arc::new(Callbacks {
            on_event,
            on_connection,
        }),
    })
}

/// Connect, handshake and reconnect until stopped.
fn manage(inner: Arc<Inner>, name: String) {
    let mut backoff = BACKOFF_START;
    while !inner.stop.load(Ordering::SeqCst) {
        if let Some((link, done)) = open_session(&inner, &name) {
            publish(&inner, link);
            wait_for_disconnect(&inner.stop, &done);
            retire(&inner);
            backoff = BACKOFF_START;
        }
        sleep_unless_stopped(&inner.stop, backoff);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    retire(&inner);
}

/// Connects, handshakes and subscribes. Returns the link and a receiver that gets a
/// message (or disconnects) when the reader ends. Returns `None` on any failure.
fn open_session(inner: &Arc<Inner>, name: &str) -> Option<(Arc<Link>, mpsc::Receiver<()>)> {
    let conn = PipeClient::connect(name, CONNECT_TIMEOUT).ok()?;
    let reader_conn = conn.try_clone().ok()?;
    let canceller = conn.try_clone().ok()?;
    let link = Arc::new(Link::new(conn, canceller));
    let (done_tx, done_rx) = mpsc::channel();

    let reader_link = Arc::clone(&link);
    let callbacks = Arc::clone(&inner.callbacks);
    let stop = Arc::clone(inner);
    let spawned = thread::Builder::new()
        .name("daemon-ipc-reader".into())
        .spawn(move || read_loop(reader_link, reader_conn, callbacks, &stop.stop, done_tx));
    if spawned.is_err() {
        link.close();
        return None;
    }

    // Every failure below closes the link. Closing cancels the reader's pending read, so a
    // daemon that accepts the pipe but never answers does not leave a thread behind.
    if handshake(inner, &link).is_none() {
        link.close();
        return None;
    }
    Some((link, done_rx))
}

/// Runs Hello and Subscribe on `link`. `None` when either step fails or is answered wrongly.
fn handshake(inner: &Inner, link: &Link) -> Option<()> {
    let hello = Command::Hello {
        proto_min: PROTOCOL_MIN_SUPPORTED,
        proto_max: PROTOCOL_VERSION,
        client: CLIENT_NAME.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
    };
    let reply = link.send(inner.next_id(), hello, HANDSHAKE_TIMEOUT).ok()?;
    if !is_hello_ack(reply) {
        return None;
    }

    let subscribe = Command::Subscribe {
        topics: DEFAULT_TOPICS.to_vec(),
    };
    let reply = link
        .send(inner.next_id(), subscribe, REQUEST_TIMEOUT)
        .ok()?;
    is_subscribed(reply).then_some(())
}

/// True when `body` is a `HelloAck` whose negotiated version this build supports.
fn is_hello_ack(body: Value) -> bool {
    matches!(
        serde_json::from_value::<Reply>(body),
        Ok(Reply::HelloAck { proto, .. })
            if (PROTOCOL_MIN_SUPPORTED..=PROTOCOL_VERSION).contains(&proto)
    )
}

/// True when `body` is a `Subscribed` reply.
fn is_subscribed(body: Value) -> bool {
    matches!(
        serde_json::from_value::<Reply>(body),
        Ok(Reply::Subscribed { .. })
    )
}

/// Makes `link` the current connection and tells the owner.
fn publish(inner: &Arc<Inner>, link: Arc<Link>) {
    *lock(&inner.link) = Some(link);
    inner.connected.store(true, Ordering::SeqCst);
    (inner.callbacks.on_connection)(true);
}

/// Drops the current connection. Reports the loss unless the client is stopping.
fn retire(inner: &Arc<Inner>) {
    let link = lock(&inner.link).take();
    if let Some(link) = link {
        link.close();
    }
    let was_connected = inner.connected.swap(false, Ordering::SeqCst);
    if was_connected && !inner.stop.load(Ordering::SeqCst) {
        (inner.callbacks.on_connection)(false);
    }
}

/// Blocks until the reader reports the connection ended, or the client is stopped.
fn wait_for_disconnect(stop: &AtomicBool, done: &mpsc::Receiver<()>) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match done.recv_timeout(POLL) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Sleeps for `duration`, waking early when the client is stopped.
fn sleep_unless_stopped(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep(POLL.min(deadline - now));
    }
}

/// Reads frames until the connection fails. Replies go to the waiting request, and
/// events go to `on_event` with `seq` added. Frames that match neither are ignored.
fn read_loop(
    link: Arc<Link>,
    mut reader: PipeConn,
    callbacks: Arc<Callbacks>,
    stop: &AtomicBool,
    done: mpsc::Sender<()>,
) {
    while let Ok(mut frame) = read_frame::<_, Value>(&mut reader) {
        // Move the body out of the frame instead of cloning it.
        let body = frame
            .get_mut("body")
            .map(Value::take)
            .unwrap_or(Value::Null);
        if let Some(id) = frame.get("id").and_then(Value::as_u64) {
            link.deliver(id, body);
        } else if let Some(seq) = frame.get("seq").and_then(Value::as_u64) {
            if stop.load(Ordering::SeqCst) {
                continue;
            }
            let mut payload = body;
            if let Value::Object(map) = &mut payload {
                map.insert("seq".into(), json!(seq));
            }
            (callbacks.on_event)(payload);
        }
    }
    link.close();
    let _ = done.send(());
}

/// Locks a mutex, recovering the data if a previous holder panicked.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Sends a daemon command and returns the reply body. Runs off the UI thread.
#[tauri::command]
pub async fn daemon_request(
    client: State<'_, Arc<DaemonClient>>,
    command: Value,
) -> Result<Value, String> {
    let timeout = timeout_for(&command);
    let client = Arc::clone(&client);
    tauri::async_runtime::spawn_blocking(move || client.request(command, timeout))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// True while the UI has a live connection to the daemon.
#[tauri::command]
pub fn daemon_connected(client: State<'_, Arc<DaemonClient>>) -> bool {
    client.is_connected()
}

/// Why the daemon connection could not be started, or `null` when it was started. Lets the
/// UI show a reason instead of a `daemon_connected` that stays `false`.
#[tauri::command]
pub fn daemon_connection_error(client: State<'_, Arc<DaemonClient>>) -> Option<String> {
    client.connection_error().map(str::to_owned)
}

/// Shutdown mode sent to the daemon (D47). Wire values "graceful" and "force".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownRequest {
    /// Refused with a `busy` error while a recording session is active.
    Graceful,
    /// Always shuts the daemon down, even while a session is recording.
    Force,
}

/// Builds the `shutdown` command for `mode`: `{"type":"shutdown","data":{"mode":..}}`.
pub fn shutdown_command(mode: ShutdownRequest) -> Value {
    let mode = match mode {
        ShutdownRequest::Graceful => "graceful",
        ShutdownRequest::Force => "force",
    };
    json!({ "type": "shutdown", "data": { "mode": mode } })
}

/// Asks the daemon to shut down and waits up to three seconds for the ack. Shared by
/// `exit_app` and the window close path in `closing`, so the two cannot drift apart.
/// A timeout or error means the daemon may still be running; callers exit the UI anyway.
/// A `busy` error reply (only for [`ShutdownRequest::Graceful`]) means a recording is active.
pub fn request_shutdown(
    client: &DaemonClient,
    mode: ShutdownRequest,
) -> Result<Value, ClientError> {
    client.request(shutdown_command(mode), SHUTDOWN_ACK_TIMEOUT)
}

/// Exits the UI. With `stop_daemon`, asks the daemon to shut down first with
/// [`ShutdownRequest::Force`]: the user already confirmed the exit dialog, so a recording
/// does not block it (see [`request_shutdown`]). Without it, only the UI process ends.
#[tauri::command]
pub async fn exit_app(
    app: AppHandle,
    client: State<'_, Arc<DaemonClient>>,
    stop_daemon: bool,
) -> Result<(), String> {
    if stop_daemon {
        let client = Arc::clone(&client);
        let _ = tauri::async_runtime::spawn_blocking(move || {
            request_shutdown(&client, ShutdownRequest::Force)
        })
        .await;
    }
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_subscription_leaves_out_levels() {
        assert!(!DEFAULT_TOPICS.contains(&Topic::Levels));
        assert_eq!(LEVELS_TOPICS, [Topic::Levels]);
        assert_eq!(DEFAULT_TOPICS.len(), 7);
        // library_changed and library_scan come on this topic; without it the gallery never
        // learns that a scan found files.
        assert!(DEFAULT_TOPICS.contains(&Topic::Library));
    }

    #[test]
    fn mic_test_uses_long_timeout() {
        assert_eq!(
            timeout_for(&json!({"type":"mic_test","data":{"device":null}})),
            LONG_REQUEST_TIMEOUT
        );
    }

    #[test]
    fn library_and_recording_commands_use_long_timeout() {
        for ty in [
            "register_edited_recording",
            "rename_recording",
            "delete_recording",
            "set_favorite",
            "scan_libraries",
            "import_plan",
            "import_start",
            "import_cancel",
            "list_libraries",
            "list_game_names",
            "clip_now",
        ] {
            assert_eq!(
                timeout_for(&json!({ "type": ty })),
                LONG_REQUEST_TIMEOUT,
                "{ty} should use the long timeout"
            );
        }
    }

    #[test]
    fn status_and_ping_use_default_timeout() {
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(15));
        assert_eq!(
            timeout_for(&json!({"type": "get_status"})),
            REQUEST_TIMEOUT
        );
        assert_eq!(timeout_for(&json!({"type": "ping"})), REQUEST_TIMEOUT);
    }

    #[test]
    fn shutdown_command_carries_mode_in_data() {
        assert_eq!(
            shutdown_command(ShutdownRequest::Graceful),
            json!({ "type": "shutdown", "data": { "mode": "graceful" } })
        );
        assert_eq!(
            shutdown_command(ShutdownRequest::Force),
            json!({ "type": "shutdown", "data": { "mode": "force" } })
        );
    }
}
