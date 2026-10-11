//! Tests for #89: the daemon client reports when its connection manager could not start.
//!
//! Only the success path is covered here. Forcing `resolve_pipe_name` to fail needs the
//! current-user SID lookup to fail, which cannot be arranged deterministically from a test.
//! No daemon is spawned: the client only connects to the (unique, absent) pipe name below.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use clipcheck_ui_lib::ipc::DaemonClient;

#[test]
fn started_client_reports_no_connection_error() {
    let pipe = format!(r"\.\pipe\clipcheck-test-issue89-{}", std::process::id());
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&connections);
    let client = DaemonClient::start(
        Some(pipe),
        Box::new(|_event| {}),
        Box::new(move |connected| {
            if connected {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        }),
    );
    assert_eq!(
        client.connection_error(),
        None,
        "an explicit pipe name must start the manager"
    );
    assert!(
        !client.is_connected(),
        "no daemon is listening on the test pipe"
    );
    assert_eq!(connections.load(Ordering::SeqCst), 0);
    client.stop();
}
