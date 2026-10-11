//! Short UI sounds. The WAV is embedded in the binary and played asynchronously
//! through the Windows multimedia API, so no call here blocks or panics.

#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use std::time::Instant;

/// Minimum spacing between two plays. A call inside this window is dropped.
#[cfg(windows)]
const MIN_INTERVAL_MS: u64 = 300;

/// Embedded clip-saved sound. The wrapper gives the buffer 4-byte alignment for
/// PlaySound; the static lives for the whole program, which covers async playback.
#[cfg(windows)]
#[repr(C, align(4))]
struct Aligned<T>(T);

#[cfg(windows)]
const CLIP_SAVED_LEN: usize = include_bytes!("../../../assets/sounds/clip-saved.wav").len();

#[cfg(windows)]
static CLIP_SAVED_WAV: Aligned<[u8; CLIP_SAVED_LEN]> =
    Aligned(*include_bytes!("../../../assets/sounds/clip-saved.wav"));

/// Time of the last accepted play, as milliseconds since [`epoch`] plus one.
/// Zero means nothing has played yet.
#[cfg(windows)]
static LAST_PLAY_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
fn epoch() -> &'static Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now)
}

/// Plays the clip-saved sound (assets/sounds/clip-saved.wav, embedded in the binary). Never blocks.
#[cfg(windows)]
pub fn play_clip_saved() {
    let now = epoch().elapsed().as_millis() as u64 + 1;
    let last = LAST_PLAY_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < MIN_INTERVAL_MS {
        return;
    }
    LAST_PLAY_MS.store(now, Ordering::Relaxed);

    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    let bytes: &'static [u8] = &CLIP_SAVED_WAV.0;
    // SAFETY: `bytes` is a 'static buffer, so it stays valid for the whole async
    // play. SND_MEMORY makes PlaySoundW treat the pointer as a WAV image.
    // SND_ASYNC returns immediately.
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(bytes.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// Plays the clip-saved sound (assets/sounds/clip-saved.wav, embedded in the binary). Never blocks.
#[cfg(not(windows))]
pub fn play_clip_saved() {}
