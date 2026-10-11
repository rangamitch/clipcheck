//! Serde round-trip checks for public state, event, effect and config types.
//! Black-box: public API only.

use clipcheck_session::{
    CommandError, Effect, EndReason, Event, ExitReason, Mode, Notice, SessionConfig, SplitReason,
    State, WriteError,
};

/// Serialize to JSON and back; assert equality. Type is passed explicitly
/// because `serde` itself is not a dev-dependency.
macro_rules! rt {
    ($t:ty, $v:expr) => {{
        let v: $t = $v;
        let json = serde_json::to_string(&v).expect("serialize");
        let back: $t = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back, "round-trip mismatch, json: {json}");
    }};
}

fn cfg_nonzero() -> SessionConfig {
    SessionConfig {
        max_part_ms: 60_000,
        max_part_bytes: 1_000_000,
        min_free_bytes: 2_000,
        join_relaunch_after_crash: true,
        join_window_ms: 30_000,
    }
}

const ALL_STATES: [State; 6] = [
    State::Idle,
    State::Armed,
    State::Recording,
    State::Splitting,
    State::Stopping,
    State::StoppedByUser,
];

const ALL_SPLIT: [SplitReason; 5] = [
    SplitReason::UserRequested,
    SplitReason::MaxDuration,
    SplitReason::MaxSize,
    SplitReason::StreamParamsChanged,
    SplitReason::ConfigChanged,
];

const ALL_END: [EndReason; 12] = [
    EndReason::UserStop,
    EndReason::GameExited,
    EndReason::GameCrashed,
    EndReason::GameSwitched,
    EndReason::Suspend,
    EndReason::DiskLow,
    EndReason::DiskFull,
    EndReason::VolumeGone,
    EndReason::WriterError,
    EndReason::ServiceShutdown,
    EndReason::Recovered,
    EndReason::Split(SplitReason::UserRequested),
];

const ALL_WRITE: [WriteError; 3] = [WriteError::DiskFull, WriteError::VolumeGone, WriteError::Io];

const ALL_NOTICE: [Notice; 5] = [
    Notice::DiskLowBlocked,
    Notice::DiskLow,
    Notice::WriteFailed(WriteError::Io),
    Notice::KeyframeTimeout,
    Notice::PartialKept,
];

#[test]
fn state_every_variant_round_trips() {
    for s in ALL_STATES {
        rt!(State, s);
    }
}

#[test]
fn state_splitting_serializes_as_plain_string() {
    let json = serde_json::to_string(&State::Splitting).unwrap();
    assert_eq!(json, "\"Splitting\"");
}

#[test]
fn state_unknown_variant_fails_to_deserialize() {
    let r = serde_json::from_str::<State>("\"Bogus\"");
    assert!(r.is_err(), "unknown variant parsed as {r:?}");
}

#[test]
fn mode_both_variants_round_trip() {
    rt!(Mode, Mode::Auto);
    rt!(Mode, Mode::Manual);
}

#[test]
fn exit_reason_variants_round_trip() {
    rt!(ExitReason, ExitReason::Normal);
    rt!(ExitReason, ExitReason::Crashed { code: 0xC000_0005 });
    rt!(ExitReason, ExitReason::Crashed { code: 1 });
    rt!(ExitReason, ExitReason::Unknown);
}

#[test]
fn split_reason_every_variant_round_trips() {
    for r in ALL_SPLIT {
        rt!(SplitReason, r);
    }
}

#[test]
fn end_reason_every_variant_round_trips() {
    for r in ALL_END {
        rt!(EndReason, r);
    }
    rt!(EndReason, EndReason::Split(SplitReason::MaxSize));
}

#[test]
fn write_error_every_variant_round_trips() {
    for w in ALL_WRITE {
        rt!(WriteError, w);
    }
}

#[test]
fn notice_every_variant_round_trips() {
    for n in ALL_NOTICE {
        rt!(Notice, n);
    }
}

#[test]
fn command_error_every_variant_round_trips() {
    rt!(CommandError, CommandError::NoTargetGame);
    rt!(CommandError, CommandError::AlreadyPending);
    rt!(CommandError, CommandError::Busy);
    rt!(CommandError, CommandError::NotEnabled);
    rt!(CommandError, CommandError::NotRecording);
    rt!(CommandError, CommandError::AlreadyRecording);
}

#[test]
fn session_config_round_trips() {
    rt!(SessionConfig, cfg_nonzero());
    rt!(SessionConfig, SessionConfig::default());
}

#[test]
fn event_every_variant_round_trips() {
    let events: Vec<Event> = vec![
        Event::SetEnabled {
            enabled: true,
            mode: Mode::Auto,
            new_session_id: "01J00000000000000000000001".into(),
        },
        Event::SetEnabled {
            enabled: false,
            mode: Mode::Manual,
            new_session_id: String::new(),
        },
        Event::Config(cfg_nonzero()),
        Event::GameStarted {
            game: "A".into(),
            now_ms: 1_700_000_000_000,
            new_session_id: "01J00000000000000000000002".into(),
        },
        Event::GameEnded {
            game: "A".into(),
            reason: ExitReason::Crashed { code: 3 },
            now_ms: 42,
        },
        Event::GameEnded {
            game: "B".into(),
            reason: ExitReason::Normal,
            now_ms: -5,
        },
        Event::GameEnded {
            game: "C".into(),
            reason: ExitReason::Unknown,
            now_ms: 0,
        },
        Event::UserStart {
            new_session_id: "01J00000000000000000000003".into(),
        },
        Event::UserStop,
        Event::UserSplit,
        Event::Shutdown,
        Event::Tick {
            now_ms: 9_000,
            part_elapsed_ms: 1_234,
            part_bytes: 5_678,
            free_bytes: 9_999,
        },
        Event::Suspend { now_ms: 77 },
        Event::KeyframeArrived,
        Event::KeyframeTimeout { attempts: 3 },
        Event::StreamParamsChanged,
        Event::LayoutChanged,
        Event::WriteFailed(WriteError::DiskFull),
        Event::WriteFailed(WriteError::VolumeGone),
        Event::WriteFailed(WriteError::Io),
        Event::FinalizeDone {
            ok: true,
            new_session_id: "01J00000000000000000000004".into(),
        },
        Event::FinalizeDone {
            ok: false,
            new_session_id: "01J00000000000000000000005".into(),
        },
    ];
    for e in events {
        rt!(Event, e);
    }
}

#[test]
fn effect_every_variant_round_trips() {
    let mut effects: Vec<Effect> = vec![
        Effect::StartEncoder {
            session_id: "01J00000000000000000000001".into(),
            part_index: 2,
            game: "A".into(),
        },
        Effect::OpenPart { index: 3 },
        Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::UserRequested),
        },
        Effect::FinalizePart {
            reason: EndReason::UserStop,
        },
        Effect::StopEncoder,
        Effect::RequestKeyframe,
        Effect::CancelSplit,
        Effect::Notify(Notice::WriteFailed(WriteError::Io)),
        Effect::Notify(Notice::KeyframeTimeout),
        Effect::Reply(Ok(())),
        Effect::Reply(Err(CommandError::Busy)),
        Effect::Reply(Err(CommandError::NoTargetGame)),
        Effect::Reply(Err(CommandError::AlreadyPending)),
    ];
    for s in ALL_STATES {
        effects.push(Effect::EmitStatus(s));
    }
    for n in ALL_NOTICE {
        effects.push(Effect::Notify(n));
    }
    for r in ALL_END {
        effects.push(Effect::FinalizePart { reason: r });
    }
    for e in effects {
        rt!(Effect, e);
    }
}

#[test]
fn unknown_event_variant_fails_to_deserialize() {
    let r = serde_json::from_str::<Event>("\"NotAnEvent\"");
    assert!(r.is_err(), "unknown variant parsed as {r:?}");
}
