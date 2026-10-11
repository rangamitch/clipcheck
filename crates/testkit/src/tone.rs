//! A process that plays a sine tone, so audio tests have a real application audio session
//! to find and capture. Replaces the old `ffplay -f lavfi sine=...` helper.
//!
//! The player is Windows PowerShell playing a WAV file in a loop through
//! `System.Media.SoundPlayer` (winmm), which opens a normal render stream owned by
//! `powershell.exe`. Nothing is downloaded or installed. The child is killed on drop.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Executable name of the tone player's audio session (for session-name assertions).
pub const TONE_PLAYER_EXE: &str = "powershell.exe";

/// Writes a 16-bit PCM stereo WAV with a quiet sine at `freq_hz` (amplitude 0.1, like the old ffplay volume 20).
pub fn write_tone_wav(path: &Path, freq_hz: f32, secs: f32, sample_rate: u32) -> io::Result<()> {
    if !(freq_hz > 0.0 && secs > 0.0 && sample_rate > 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad tone parameters",
        ));
    }
    let channels: u16 = 2;
    let frames = (secs * sample_rate as f32).round() as u32;
    let data_len = frames * u32::from(channels) * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / sample_rate as f32;
        let v =
            (0.1 * (2.0 * std::f32::consts::PI * freq_hz * t).sin() * f32::from(i16::MAX)) as i16;
        for _ in 0..channels {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(path)?.write_all(&out)
}

/// A running tone player. Killed (and reaped) on drop.
pub struct TonePlayer {
    child: Child,
    _wav: PathBuf,
}

impl TonePlayer {
    /// Writes a looping tone WAV into `dir` and starts playing it in a hidden PowerShell
    /// process for at most `max_secs` seconds.
    pub fn spawn(dir: &Path, freq_hz: f32, max_secs: u32) -> io::Result<TonePlayer> {
        let wav = dir.join(format!("tone-{}.wav", freq_hz.round() as u32));
        write_tone_wav(&wav, freq_hz, 2.0, 48_000)?;
        let path = wav.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$p = New-Object System.Media.SoundPlayer '{path}'; $p.Load(); $p.PlayLooping(); Start-Sleep -Seconds {max_secs}"
        );
        let mut cmd = Command::new(TONE_PLAYER_EXE);
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn()?;
        Ok(TonePlayer { child, _wav: wav })
    }

    /// Process id of the player (its audio session belongs to this pid).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// True while the player process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for TonePlayer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
