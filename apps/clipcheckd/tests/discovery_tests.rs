//! Discovery tests for the Settings pickers (`clipcheckd::discovery`).
//!
//! - Pure tests (`build_*`, `vendor_name`, `encoders_reply`, `detect_config_from`, `excluded_exes`)
//!   use fabricated snapshots and touch no OS state.
//! - Daemon tests spawn the real `clipcheckd` binary and talk to it over the named pipe, the same
//!   way `daemon.rs` does. They hold `TEST_LOCK` because the single-instance mutex is session-wide.
//!   Tests that need an audio endpoint, native test media, or an interactive desktop print `SKIP:` and return
//!   when those are missing.
//!
//! Windows only.

#![cfg(windows)]

use std::collections::HashMap;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_audio::SeenApp;
use clipcheck_detect::DetectConfig;
use clipcheck_encode::probe::{Backend, CodecKind, EncoderInfo, ProbeReport};
use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{
    AudioAppDto, Command as Cmd, Envelope, Event, GpuDto, Push, Reply, RunningAppDto, Topic,
};
use clipcheck_ipc::PipeConn;
use clipcheck_testkit::tone::{TonePlayer, TONE_PLAYER_EXE};
use clipcheckd::discovery::{
    build_audio_apps, build_running_apps, detect_config_from, encoders_reply, excluded_exes,
    vendor_name, LiveMap, WindowCandidate, MAX_RUNNING_APPS, MIN_WINDOW_PX, SUGGEST_SCORE,
};
use serde_json::{json, Value};

mod common;

use common::window::TestWindow;
use common::*;

fn list_running(conn: &mut PipeConn, id: u64) -> Vec<RunningAppDto> {
    match call(conn, id, Cmd::ListRunningApps).body {
        Reply::RunningApps { apps } => apps,
        other => panic!("expected RunningApps, got {other:?}"),
    }
}

fn list_audio(conn: &mut PipeConn, id: u64) -> Vec<AudioAppDto> {
    match call(conn, id, Cmd::ListAudioApps).body {
        Reply::AudioApps { apps } => apps,
        other => panic!("expected AudioApps, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A. Pure tests (fabricated snapshots, no OS)
// ---------------------------------------------------------------------------

fn cand(exe: &str, title: &str) -> WindowCandidate {
    WindowCandidate {
        exe: exe.into(),
        title: title.into(),
        pid: 1000,
        width: 800,
        height: 600,
        visible: true,
        cloaked: false,
        tool_window: false,
        blocked: false,
        system_shell: false,
        launcher: false,
        is_self: false,
        score: 0,
        known: false,
        in_library: false,
    }
}

fn with(f: impl FnOnce(&mut WindowCandidate)) -> WindowCandidate {
    let mut c = cand("bad.exe", "bad");
    f(&mut c);
    c
}

fn exes(rows: &[RunningAppDto]) -> Vec<&str> {
    rows.iter().map(|r| r.exe.as_str()).collect()
}

fn row<'a>(rows: &'a [RunningAppDto], exe: &str) -> &'a RunningAppDto {
    rows.iter()
        .find(|r| r.exe == exe)
        .unwrap_or_else(|| panic!("no row for {exe}; rows: {:?}", exes(rows)))
}

#[test]
fn running_apps_drops_non_candidate_windows() {
    let cases = vec![
        ("invisible", with(|c| c.visible = false)),
        ("cloaked", with(|c| c.cloaked = true)),
        ("tool window", with(|c| c.tool_window = true)),
        ("blocked", with(|c| c.blocked = true)),
        ("system shell", with(|c| c.system_shell = true)),
        ("launcher", with(|c| c.launcher = true)),
        ("is_self", with(|c| c.is_self = true)),
        ("narrow", with(|c| c.width = MIN_WINDOW_PX - 1)),
        ("short", with(|c| c.height = MIN_WINDOW_PX - 1)),
        ("empty exe", with(|c| c.exe.clear())),
    ];
    for (label, c) in cases {
        let rows = build_running_apps(&[c], &[], 60);
        assert!(
            rows.is_empty(),
            "{label} window was kept: {:?}",
            exes(&rows)
        );
    }

    let rows = build_running_apps(&[cand("ok.exe", "ok")], &[], 60);
    assert_eq!(exes(&rows), vec!["ok.exe"], "plain window must be kept");

    // Size boundary: exactly MIN_WINDOW_PX on both sides is kept.
    let edge = with(|c| {
        c.exe = "edge.exe".into();
        c.width = MIN_WINDOW_PX;
        c.height = MIN_WINDOW_PX;
    });
    let rows = build_running_apps(&[edge], &[], 60);
    assert_eq!(
        exes(&rows),
        vec!["edge.exe"],
        "window at exactly MIN_WINDOW_PX"
    );
}

#[test]
fn running_apps_dedupes_per_exe_topmost_wins() {
    let top = with(|c| {
        c.exe = "Game.exe".into();
        c.title = "Top title".into();
        c.pid = 1;
    });
    let second = with(|c| {
        c.exe = "game.exe".into();
        c.title = "Second".into();
        c.pid = 2;
    });
    let other = with(|c| {
        c.exe = "Other.exe".into();
        c.title = "Other".into();
    });
    let rows = build_running_apps(&[top, second, other], &[], 60);
    assert_eq!(rows.len(), 2, "rows: {:?}", exes(&rows));
    let g = row(&rows, "Game.exe");
    assert_eq!(g.name, "Top title", "topmost title must win");
    assert_eq!(g.pid, 1, "topmost pid must win");
}

#[test]
fn running_apps_excluded_list_is_case_insensitive() {
    let rows = build_running_apps(
        &[cand("DISCORD.exe", "d"), cand("Keep.exe", "k")],
        &["Discord.EXE".to_string()],
        60,
    );
    assert_eq!(exes(&rows), vec!["Keep.exe"]);

    let rows = build_running_apps(
        &[cand("Discord.exe", "d")],
        &["discord.exe".to_string()],
        60,
    );
    assert!(
        rows.is_empty(),
        "lowercase exclusion did not match: {:?}",
        exes(&rows)
    );
}

#[test]
fn running_apps_suggested_and_game_thresholds() {
    assert_eq!(SUGGEST_SCORE, 30, "SUGGEST_SCORE changed; update this test");
    let mk = |exe: &str, score: u32, known: bool, lib: bool| {
        let mut c = cand(exe, exe);
        c.score = score;
        c.known = known;
        c.in_library = lib;
        c
    };
    let cands = vec![
        mk("s29.exe", 29, false, false),
        mk("s30.exe", 30, false, false),
        mk("s59.exe", 59, false, false),
        mk("s60.exe", 60, false, false),
        mk("known.exe", 0, true, false),
        mk("lib.exe", 0, false, true),
    ];
    let rows = build_running_apps(&cands, &[], 60);
    let check = |exe: &str, suggested: bool, is_game: bool| {
        let r = row(&rows, exe);
        assert_eq!(
            (r.suggested, r.is_game),
            (suggested, is_game),
            "{exe}: (suggested, is_game)"
        );
    };
    check("s29.exe", false, false);
    check("s30.exe", true, false);
    check("s59.exe", true, false);
    check("s60.exe", true, true);
    check("known.exe", true, true);
    check("lib.exe", true, true);
}

#[test]
fn running_apps_ordering_suggested_then_name_then_exe() {
    let zeta = with(|c| {
        c.exe = "zeta.exe".into();
        c.title = "Zed".into();
        c.score = 40;
    });
    let mid = with(|c| {
        c.exe = "mid.exe".into();
        c.title = "mid".into();
    });
    let apple = with(|c| {
        c.exe = "Apple.exe".into();
        c.title = "apple".into();
    });
    let bb = with(|c| {
        c.exe = "bb.exe".into();
        c.title = "Beta".into();
    });
    let aa = with(|c| {
        c.exe = "aa.exe".into();
        c.title = "Beta".into();
    });
    let rows = build_running_apps(&[zeta, mid, apple, bb, aa], &[], 60);
    assert_eq!(
        exes(&rows),
        vec!["zeta.exe", "Apple.exe", "aa.exe", "bb.exe", "mid.exe"],
        "suggested first, then name case-insensitively, then exe"
    );
}

#[test]
fn running_apps_name_is_trimmed_title_or_exe_stem() {
    let rows = build_running_apps(
        &[
            cand("a.exe", "  Spaced Title  "),
            cand("Foo.exe", "   "),
            cand("Bar.EXE", ""),
        ],
        &[],
        60,
    );
    assert_eq!(row(&rows, "a.exe").name, "Spaced Title");
    assert_eq!(row(&rows, "Foo.exe").name, "Foo");
    assert_eq!(row(&rows, "Bar.EXE").name, "Bar");
}

#[test]
fn running_apps_capped_at_max_rows() {
    let cands: Vec<WindowCandidate> = (0..MAX_RUNNING_APPS + 10)
        .map(|i| cand(&format!("app{i:03}.exe"), &format!("app {i}")))
        .collect();
    let rows = build_running_apps(&cands, &[], 60);
    assert_eq!(rows.len(), MAX_RUNNING_APPS);
}

fn seen(exe: &str, display: &str, last_ms: u64) -> SeenApp {
    SeenApp {
        exe_name: exe.into(),
        display_name: display.into(),
        exe_path: None,
        first_seen_ms: 1,
        last_seen_ms: last_ms,
    }
}

#[test]
fn audio_apps_keep_silent_memory_and_mark_live_sessions() {
    let seen_list = vec![
        seen("Discord.exe", "  Discord  ", 1000),
        seen("Spotify.exe", "", 500),
        seen("clipcheckd.exe", "x", 9),
        seen("ClipCheck.exe", "x", 9),
        seen("CLIPCHECK-ui.exe", "x", 9),
        seen("", "nameless", 9),
        seen("nan.exe", "", 700),
        seen("loud.exe", "", 800),
        seen("neg.exe", "", 600),
    ];
    let mut live: LiveMap = HashMap::new();
    live.insert("discord.exe".into(), (true, 0.5));
    live.insert("nan.exe".into(), (true, f32::NAN));
    live.insert("loud.exe".into(), (true, 3.0));
    live.insert("neg.exe".into(), (true, -1.0));

    let rows = build_audio_apps(&seen_list, &live);
    let got: Vec<&str> = rows.iter().map(|r| r.exe.as_str()).collect();
    assert_eq!(
        got,
        vec![
            "Discord.exe",
            "Spotify.exe",
            "nan.exe",
            "loud.exe",
            "neg.exe"
        ],
        "clipcheck* and empty exes must be excluded"
    );

    let d = &rows[0];
    assert!(d.active);
    assert_eq!(d.peak, 0.5);
    assert_eq!(d.name, "Discord");
    assert_eq!(d.last_active_unix_ms, 1000);
    assert!(d.icon_png_b64.is_none());

    let s = &rows[1];
    assert!(!s.active, "silent remembered app must be inactive");
    assert_eq!(s.peak, 0.0);
    assert_eq!(
        s.name, "Spotify",
        "empty display name falls back to exe stem"
    );
    assert_eq!(s.last_active_unix_ms, 500);

    assert_eq!(rows[2].peak, 0.0, "NaN peak must become 0");
    assert!(rows[2].active);
    assert_eq!(rows[3].peak, 1.0, "peak above 1 must clamp to 1");
    assert_eq!(rows[4].peak, 0.0, "negative peak must clamp to 0");
}

#[test]
fn encoders_reply_without_probe_report() {
    let gpus = vec![GpuDto {
        name: "NVIDIA GeForce RTX 4070".into(),
        vendor: "nvidia".into(),
        index: 0,
        dedicated_mb: 12288,
    }];
    match encoders_reply(None, gpus.clone()) {
        Reply::Encoders {
            encoder_ready,
            gpus: got,
            encoders,
        } => {
            assert!(
                !encoder_ready,
                "no probe report must mean encoder_ready false"
            );
            assert!(encoders.is_empty());
            assert_eq!(got, gpus, "gpus must be preserved");
        }
        other => panic!("expected Encoders, got {other:?}"),
    }
}

#[test]
fn encoders_reply_maps_probe_report() {
    let report = ProbeReport {
        build: "test build 8.1".into(),
        encoders: vec![
            EncoderInfo {
                backend: Backend::Nvenc,
                codec: CodecKind::H264,
                encoder_name: "h264_nvenc".into(),
                works: true,
                error: None,
                probe_ms: 1,
            },
            EncoderInfo {
                backend: Backend::Software,
                codec: CodecKind::H265,
                encoder_name: "libx265".into(),
                works: false,
                error: Some("not built in".into()),
                probe_ms: 2,
            },
        ],
    };
    match encoders_reply(Some(&report), Vec::new()) {
        Reply::Encoders {
            encoder_ready,
            encoders,
            ..
        } => {
            assert!(encoder_ready);
            assert_eq!(encoders.len(), 2);
            assert_eq!(encoders[0].backend, "nvenc");
            assert_eq!(encoders[0].codec, "h264");
            assert_eq!(encoders[0].encoder_name, "h264_nvenc");
            assert!(encoders[0].works);
            assert_eq!(encoders[1].backend, "software");
            assert_eq!(encoders[1].codec, "h265");
            assert!(!encoders[1].works);
        }
        other => panic!("expected Encoders, got {other:?}"),
    }
}

#[test]
fn vendor_name_maps_pci_ids() {
    assert_eq!(vendor_name(0x10DE), "nvidia");
    assert_eq!(vendor_name(0x1002), "amd");
    assert_eq!(vendor_name(0x1022), "amd");
    assert_eq!(vendor_name(0x8086), "intel");
    assert_eq!(vendor_name(0x1414), "other");
    assert_eq!(vendor_name(0), "other");
}

#[test]
fn detect_config_reads_allow_and_block_lists() {
    let cfg = json!({"detection": {
        "allow": ["Game.exe", {"exe": "Other.exe"}, 5],
        "block": ["Launcher.exe", {"exe": "Blocked.exe"}]
    }});
    let base = DetectConfig::default();
    let min = base.min_score;
    let out = detect_config_from(&cfg, base);
    let allow: Vec<&str> = out.allow.iter().map(|a| a.exe.as_str()).collect();
    assert_eq!(
        allow,
        vec!["Game.exe", "Other.exe"],
        "non-string/non-object entries dropped"
    );
    assert_eq!(
        out.block_extra,
        vec!["Launcher.exe".to_string(), "Blocked.exe".to_string()]
    );
    assert_eq!(out.min_score, min, "thresholds must come from base");

    let empty = detect_config_from(&json!({}), DetectConfig::default());
    assert!(empty.allow.is_empty());
    assert!(empty.block_extra.is_empty());
}

#[test]
fn excluded_exes_lowercases_and_appends_attached() {
    let cfg = json!({"detection": {"allow": ["Game.exe", {"exe": "Other.EXE"}]}});
    let ex = excluded_exes(&cfg, Some("Attached.exe"));
    for want in ["game.exe", "other.exe", "attached.exe"] {
        assert!(ex.iter().any(|e| e == want), "{want} missing: {ex:?}");
    }
    assert!(
        ex.iter().all(|e| *e == e.to_lowercase()),
        "not lowercase: {ex:?}"
    );
    assert_eq!(excluded_exes(&cfg, None).len(), 2);
}

// ---------------------------------------------------------------------------
// D. Daemon integration
// ---------------------------------------------------------------------------

/// Starts a reader thread that owns `conn` and forwards every frame as a raw JSON value.
/// The thread exits on a read error or when the receiver is dropped (checked on the next frame).
fn spawn_reader(mut conn: PipeConn) -> mpsc::Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match read_frame::<_, Value>(&mut conn) {
            Ok(v) => {
                if tx.send(Ok(v)).is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(format!("{e:?}")));
                break;
            }
        }
    });
    rx
}

/// Returns the microphone level if the frame is a `mic_level` push. Replies are ignored.
fn as_mic_level(v: &Value) -> Option<(f32, f32)> {
    v.get("seq")?;
    let p: Push = serde_json::from_value(v.clone()).ok()?;
    match p.body {
        Event::MicLevel { rms_db, peak_db } => Some((rms_db, peak_db)),
        _ => None,
    }
}

/// Collects `mic_level` samples from the reader for `dur`.
fn mic_levels_for(rx: &mpsc::Receiver<Result<Value, String>>, dur: Duration) -> Vec<(f32, f32)> {
    let deadline = Instant::now() + dur;
    let mut out = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(Ok(v)) => {
                if let Some(s) = as_mic_level(&v) {
                    out.push(s);
                }
            }
            Ok(Err(e)) => {
                println!("reader ended: {e}");
                break;
            }
            Err(_) => break,
        }
    }
    out
}

#[test]
fn list_encoders_without_probe() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(&mut conn, 2, Cmd::ListEncoders).body {
        Reply::Encoders {
            encoder_ready,
            gpus,
            encoders,
        } => {
            assert!(
                !encoder_ready,
                "--no-encoders must report encoder_ready false"
            );
            assert!(encoders.is_empty(), "no encoders expected: {encoders:?}");
            for g in &gpus {
                assert!(
                    ["nvidia", "amd", "intel", "other"].contains(&g.vendor.as_str()),
                    "unexpected vendor {:?} for {}",
                    g.vendor,
                    g.name
                );
            }
            println!(
                "D1 gpus={} ({:?})",
                gpus.len(),
                gpus.iter().map(|g| &g.vendor).collect::<Vec<_>>()
            );
        }
        other => panic!("expected Encoders, got {other:?}"),
    }
}

#[test]
fn list_audio_devices_has_one_default_render() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(&mut conn, 2, Cmd::ListAudioDevices).body {
        Reply::AudioDevices { capture, render } => {
            println!("D2 capture={} render={}", capture.len(), render.len());
            if !render.is_empty() {
                let defaults = render.iter().filter(|x| x.is_default).count();
                assert_eq!(
                    defaults, 1,
                    "expected exactly one default render device, found {defaults}: {render:?}"
                );
            }
        }
        other => panic!("expected AudioDevices, got {other:?}"),
    }
}

#[test]
fn list_running_apps_shows_this_test_window() {
    let _g = lock();
    let window = match TestWindow::create() {
        Ok(w) => w,
        Err(e) => {
            println!("SKIP: no window for list_running_apps test: {e}");
            return;
        }
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    let me = std::env::current_exe()
        .expect("current_exe")
        .file_name()
        .expect("exe file name")
        .to_string_lossy()
        .to_string();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut id = 2u64;
    let mut last: Vec<RunningAppDto>;
    let mut found = false;
    loop {
        last = list_running(&mut conn, id);
        id += 1;
        if last.iter().any(|a| a.exe.eq_ignore_ascii_case(&me)) {
            found = true;
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    assert!(
        found,
        "test exe {me} not in running apps after 10 s; exes seen: {:?}",
        exes(&last)
    );
    let mine = last
        .iter()
        .find(|a| a.exe.eq_ignore_ascii_case(&me))
        .expect("found above");
    println!("D3 running apps={} name={:?}", last.len(), mine.name);
    assert!(!mine.name.trim().is_empty(), "name is empty");
    let icon = mine
        .icon_png_b64
        .as_deref()
        .unwrap_or_else(|| panic!("icon_png_b64 is None for {me}"));
    assert!(!icon.is_empty(), "icon_png_b64 is empty");
    assert_png_32(&b64_decode(icon));

    for a in &last {
        assert!(!a.exe.is_empty(), "empty exe in {a:?}");
        assert!(!a.name.is_empty(), "empty name in {a:?}");
        assert!(
            !a.exe.to_lowercase().starts_with("clipcheck"),
            "clipcheck exe listed: {}",
            a.exe
        );
    }
    drop(window);
}

#[test]
fn list_audio_apps_tracks_tone_player() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(&mut conn, 2, Cmd::ListAudioDevices).body {
        Reply::AudioDevices { render, .. } => {
            if render.is_empty() {
                println!("SKIP: no render endpoint, cannot observe a tone");
                return;
            }
        }
        other => panic!("expected AudioDevices, got {other:?}"),
    }
    // First call starts the audio poller; the tone is seen on later polls.
    let _ = list_audio(&mut conn, 3);

    let mut tone = TonePlayer::spawn(dir.path(), 440.0, 12).expect("spawn tone player");

    let state_of = |apps: &[AudioAppDto]| {
        apps.iter()
            .find(|a| a.exe.eq_ignore_ascii_case(TONE_PLAYER_EXE))
            .map(|a| (a.active, a.last_active_unix_ms))
    };
    let mut timeline = String::new();
    let mut id = 10u64;
    let t0 = Instant::now();
    let mut active_seen = false;
    while t0.elapsed() < Duration::from_secs(10) {
        let apps = list_audio(&mut conn, id);
        id += 1;
        for a in &apps {
            assert!(
                !a.exe.to_lowercase().starts_with("clipcheck"),
                "clipcheck exe listed: {}",
                a.exe
            );
        }
        let st = state_of(&apps);
        timeline.push_str(match st {
            Some((true, _)) => "A ",
            Some((false, _)) => "i ",
            None => "- ",
        });
        if matches!(st, Some((true, _))) {
            active_seen = true;
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    assert!(
        active_seen,
        "tone player never reported active within 10 s; timeline: {timeline}"
    );

    // Wait for the tone player to finish, then expect the entry to remain with active == false.
    let t_wait = Instant::now();
    while tone.is_running() && t_wait.elapsed() < Duration::from_secs(15) {
        thread::sleep(Duration::from_millis(100));
    }
    assert!(!tone.is_running(), "tone player still running after 15 s");
    println!("tone player exited");
    let t1 = Instant::now();
    let mut inactive_seen = false;
    let mut last_active_ms: i64 = 0;
    while t1.elapsed() < Duration::from_secs(8) {
        let apps = list_audio(&mut conn, id);
        id += 1;
        for a in &apps {
            assert!(
                !a.exe.to_lowercase().starts_with("clipcheck"),
                "clipcheck exe listed: {}",
                a.exe
            );
        }
        let st = state_of(&apps);
        timeline.push_str(match st {
            Some((true, _)) => "A ",
            Some((false, _)) => "i ",
            None => "- ",
        });
        if let Some((active, last)) = st {
            last_active_ms = last_active_ms.max(last);
            if !active {
                inactive_seen = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    println!("D4 timeline (A=active i=inactive -=absent): {timeline}");
    assert!(
        inactive_seen,
        "tone player entry never showed active == false after exit; timeline: {timeline}"
    );
    assert!(
        last_active_ms > 0,
        "last_active_unix_ms not set for tone player: {last_active_ms}"
    );
}

#[test]
fn mic_meter_streams_and_stops_when_owner_disconnects() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(&mut conn, 2, Cmd::ListAudioDevices).body {
        Reply::AudioDevices { capture, .. } => {
            if capture.is_empty() {
                println!("SKIP: no capture endpoint for mic_meter");
                return;
            }
        }
        other => panic!("expected AudioDevices, got {other:?}"),
    }

    match call(
        &mut conn,
        3,
        Cmd::MicMeter {
            enable: true,
            device: None,
        },
    )
    .body
    {
        Reply::Ok => {}
        other => panic!("mic_meter enable expected Ok, got {other:?}"),
    }
    match call(
        &mut conn,
        4,
        Cmd::Subscribe {
            topics: vec![Topic::Meter],
        },
    )
    .body
    {
        Reply::Subscribed { .. } => {}
        other => panic!("expected Subscribed, got {other:?}"),
    }

    // Owner connection: reader thread owns a clone, this thread keeps the writer for refreshes.
    let mut writer = conn;
    let rx = spawn_reader(writer.try_clone().expect("clone conn"));
    let mut samples = Vec::new();
    for id in (100u64..).take(2) {
        write_frame(
            &mut writer,
            &Envelope::request(
                id,
                Cmd::MicMeter {
                    enable: true,
                    device: None,
                },
            ),
        )
        .expect("refresh mic_meter");
        samples.extend(mic_levels_for(&rx, Duration::from_millis(500)));
    }
    println!("D5 owner samples in 1 s: {}", samples.len());
    assert!(
        samples.len() >= 3,
        "expected >= 3 mic_level events in 1 s, got {}",
        samples.len()
    );
    for (rms, peak) in &samples {
        assert!(
            rms.is_finite() && (-100.0..=0.0).contains(rms),
            "rms_db out of range: {rms}"
        );
        assert!(
            peak.is_finite() && (-100.0..=0.0).contains(peak),
            "peak_db out of range: {peak}"
        );
    }

    // Disconnect the owner: drop the receiver so the reader exits on its next frame, then drop
    // the writer. Together these close every handle of the owner connection.
    drop(rx);
    drop(writer);
    thread::sleep(Duration::from_millis(300));

    // A new owner subscribes but never starts a meter. It must see no samples.
    let mut conn2 = connect_and_hello(&mut d, &pipe);
    match call(
        &mut conn2,
        2,
        Cmd::Subscribe {
            topics: vec![Topic::Meter],
        },
    )
    .body
    {
        Reply::Subscribed { .. } => {}
        other => panic!("expected Subscribed, got {other:?}"),
    }
    let rx2 = spawn_reader(conn2);
    let after = mic_levels_for(&rx2, Duration::from_millis(1500));
    assert!(
        after.is_empty(),
        "meter kept running after owner disconnected: {} samples",
        after.len()
    );
}

#[test]
fn mic_meter_disable_stops_events() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(&mut conn, 2, Cmd::ListAudioDevices).body {
        Reply::AudioDevices { capture, .. } => {
            if capture.is_empty() {
                println!("SKIP: no capture endpoint for mic_meter");
                return;
            }
        }
        other => panic!("expected AudioDevices, got {other:?}"),
    }
    match call(
        &mut conn,
        3,
        Cmd::MicMeter {
            enable: true,
            device: None,
        },
    )
    .body
    {
        Reply::Ok => {}
        other => panic!("mic_meter enable expected Ok, got {other:?}"),
    }
    match call(
        &mut conn,
        4,
        Cmd::Subscribe {
            topics: vec![Topic::Meter],
        },
    )
    .body
    {
        Reply::Subscribed { .. } => {}
        other => panic!("expected Subscribed, got {other:?}"),
    }

    let mut writer = conn;
    let rx = spawn_reader(writer.try_clone().expect("clone conn"));
    let on = mic_levels_for(&rx, Duration::from_millis(1000));
    assert!(
        on.len() >= 3,
        "expected >= 3 samples while enabled, got {}",
        on.len()
    );

    write_frame(
        &mut writer,
        &Envelope::request(
            50,
            Cmd::MicMeter {
                enable: false,
                device: None,
            },
        ),
    )
    .expect("disable mic_meter");
    // Allow in-flight samples to drain, then expect silence.
    let _ = mic_levels_for(&rx, Duration::from_millis(300));
    let off = mic_levels_for(&rx, Duration::from_millis(1000));
    assert!(
        off.is_empty(),
        "samples kept arriving after mic_meter enable=false: {}",
        off.len()
    );
    drop(rx);
    drop(writer);
}

#[test]
fn list_replies_return_quickly() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    let t = Instant::now();
    let _ = list_running(&mut conn, 2);
    let running_ms = t.elapsed().as_millis();
    assert!(running_ms < 3000, "ListRunningApps took {running_ms} ms");

    let t = Instant::now();
    let _ = list_audio(&mut conn, 3);
    let audio_ms = t.elapsed().as_millis();
    assert!(audio_ms < 3000, "ListAudioApps took {audio_ms} ms");
    println!("D6 running={running_ms} ms audio={audio_ms} ms");
}

// ---------------------------------------------------------------------------
// mic_test (Test microphone)
// ---------------------------------------------------------------------------

#[test]
fn mic_test_reply_bogus_device_is_error_not_panic() {
    let reply = clipcheckd::discovery::mic_test_reply(Some("no-such-device-id".to_owned()));
    match reply {
        Reply::Error { .. } => {}
        other => panic!("expected Reply::Error for a bogus device id, got {other:?}"),
    }
}

#[cfg(windows)]
#[test]
fn mic_test_reply_default_device_returns_five_second_wav() {
    use base64::Engine;

    let captures = clipcheck_audio::list_endpoints(clipcheck_audio::EndpointKind::Capture)
        .expect("list capture endpoints");
    if captures.is_empty() {
        println!("SKIP: no capture endpoint for mic_test_reply");
        return;
    }

    match clipcheckd::discovery::mic_test_reply(None) {
        Reply::MicTest {
            wav_base64,
            duration_ms,
            peak_db,
        } => {
            assert_eq!(duration_ms, 5000, "duration_ms");
            assert!(peak_db.is_finite(), "peak_db not finite: {peak_db}");
            let wav = base64::engine::general_purpose::STANDARD
                .decode(wav_base64.as_bytes())
                .expect("wav_base64 decodes");
            assert!(wav.starts_with(b"RIFF"), "not a RIFF file");
            assert_eq!(&wav[8..12], b"WAVE", "missing WAVE tag");
            assert_eq!(wav.len(), 44 + 2 * 240_000, "wav length");
        }
        Reply::Error { message, .. } => {
            // A capture endpoint can exist yet refuse to open (e.g. privacy setting).
            println!("SKIP: capture endpoint exists but recording failed: {message}");
        }
        other => panic!("expected Reply::MicTest, got {other:?}"),
    }
}
