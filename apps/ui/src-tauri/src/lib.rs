//! ClipCheck desktop UI: Tauri v2 shell and Gallery commands.

use std::sync::{Arc, OnceLock};

use serde_json::json;
use tauri::{Emitter, Manager};

pub mod cli;
pub mod closing;
pub mod commands;
pub mod daemon;
pub mod details;
pub mod fileops;
pub mod ipc;
pub mod media;
pub mod mediaserver;
pub mod player;
pub mod trim;
pub mod update_state;
pub mod updater;

/// Debug builds with `CLIPCHECK_DEV_SEED=1` create a fake library on first start, if none exists.
#[cfg(debug_assertions)]
fn dev_seed_on_startup() {
    let db = match commands::try_default_db_path() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("clipcheck-ui: dev seed skipped: {e}");
            return;
        }
    };
    match commands::maybe_dev_seed(&db) {
        Ok(true) => eprintln!("clipcheck-ui: seeded dev library"),
        Ok(false) => {}
        Err(e) => eprintln!("clipcheck-ui: dev seed failed: {e}"),
    }
}

#[cfg(not(debug_assertions))]
fn dev_seed_on_startup() {}

/// Starts the Tauri application.
pub fn run() {
    tauri::Builder::default()
        // Must be the first plugin. A second launch focuses the existing window and exits;
        // the recorder daemon is a separate process and is not affected.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        // In-app updates (see `updater`). The plugin config is `plugins.updater` in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            dev_seed_on_startup();
            // Loopback streaming media server; started now so the first open is instant.
            let _ = mediaserver::start();
            // Connects to the recorder daemon in the background and reconnects on its own.
            // Events reach every window as `daemon-event`, and connection changes as
            // `daemon-connection` with `{ "connected": bool }`.
            // Close settings are cached from the daemon's config (see `closing`).
            app.manage(closing::new_cache());
            // The callbacks run on the connection thread, which can start before this setup
            // reaches `manage`. They read the client from this slot, filled right after start,
            // so they never poll for managed state.
            let slot: Arc<OnceLock<Arc<ipc::DaemonClient>>> = Arc::new(OnceLock::new());
            let event_slot = Arc::clone(&slot);
            let connection_slot = Arc::clone(&slot);
            let events = app.handle().clone();
            let connection = app.handle().clone();
            let client = Arc::new(ipc::DaemonClient::start(
                None,
                Box::new(move |payload| {
                    let _ = events.emit("daemon-event", payload.clone());
                    closing::on_daemon_event(&events, &payload, event_slot.get());
                }),
                Box::new(move |connected| {
                    let _ = connection.emit("daemon-connection", json!({ "connected": connected }));
                    if connected {
                        if let Some(client) = connection_slot.get() {
                            closing::refresh_in_background(connection.clone(), Arc::clone(client));
                        }
                    }
                }),
            ));
            let _ = slot.set(Arc::clone(&client));
            app.manage(Arc::clone(&client));
            // A connection made before the slot was filled was not refreshed by the callback,
            // so refresh once now. A connection made later reaches the callback.
            if client.is_connected() {
                closing::refresh_in_background(app.handle().clone(), Arc::clone(&client));
            }
            // Update state and the background check loop (reads the client managed above).
            updater::setup(app.handle());
            // Set explicitly so the logo also shows in dev runs, where the bundle icon is not applied.
            if let Some(window) = app.get_webview_window("main") {
                window.set_icon(tauri::include_image!("icons/icon.png"))?;
            }
            Ok(())
        })
        // The X button. The decision can need daemon requests, so it runs off the event loop.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let app = window.app_handle().clone();
                    std::thread::spawn(move || closing::handle_close_request(&app));
                }
            }
        })
        .register_uri_scheme_protocol("clip", |_ctx, request| clip_protocol(request))
        .invoke_handler(tauri::generate_handler![
            mediaserver::media_base_url,
            player::open_player,
            player::prepare_audio,
            details::get_recording_details,
            trim::trim_clip,
            trim::cancel_trim,
            trim::get_trim_preview,
            commands::list_recordings,
            commands::get_library_stats,
            commands::get_game_icons,
            fileops::reveal_recording,
            fileops::open_folder,
            fileops::pick_folder,
            ipc::daemon_request,
            ipc::daemon_connected,
            ipc::exit_app,
            updater::check_for_update,
            updater::install_update,
            updater::skip_update_version,
            updater::get_update_state,
            ipc::daemon_connection_error
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClipCheck");
}

/// Answers `clip://` requests (served as `http://clip.localhost/...` on Windows).
fn clip_protocol(request: tauri::http::Request<Vec<u8>>) -> tauri::http::Response<Vec<u8>> {
    let range = request
        .headers()
        .get(tauri::http::header::RANGE)
        .and_then(|v| v.to_str().ok());
    let cache_dir = player::cache_dir_from(
        std::env::var("CLIPCHECK_CACHE_DIR").ok().as_deref(),
        std::env::var("LOCALAPPDATA").ok().as_deref(),
    );
    let db = match commands::try_default_db_path() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("clipcheck-ui: clip request failed: {e}");
            let mut response = tauri::http::Response::new(Vec::new());
            *response.status_mut() = tauri::http::StatusCode::INTERNAL_SERVER_ERROR;
            return response;
        }
    };
    let out = media::serve_with_method(
        &db,
        &cache_dir,
        request.method().as_str(),
        request.uri().path(),
        range,
    );
    let mut builder = tauri::http::Response::builder().status(out.status);
    for (name, value) in &out.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(out.body)
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}
