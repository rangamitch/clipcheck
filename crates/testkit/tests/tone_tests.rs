//! Tone helper: WAV layout and the player's lifetime.

use clipcheck_testkit::tone::{write_tone_wav, TonePlayer};
use std::time::{Duration, Instant};

#[test]
fn wav_has_pcm_header_and_expected_size() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.wav");
    write_tone_wav(&p, 440.0, 1.0, 48_000).unwrap();
    let b = std::fs::read(&p).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..16], b"WAVEfmt ");
    assert_eq!(b.len(), 44 + 48_000 * 2 * 2);
    assert!(write_tone_wav(&p, 0.0, 1.0, 48_000).is_err());
}

#[cfg(windows)]
#[test]
fn player_runs_and_is_killed_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let mut player = TonePlayer::spawn(dir.path(), 660.0, 20).unwrap();
    let pid = player.pid();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(player.is_running(), "tone player exited early");
    drop(player);
    // The process must be gone shortly after drop.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        if !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tone player {pid} still running after drop"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
