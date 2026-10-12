//! Public-API tests for `clipcheck_render::dsp` (pure audio DSP). Synthetic signals only.
//! Samples are interleaved f32 in -1.0..=1.0. Never executed by the tester chain; compile-checked.

use std::f64::consts::PI;

use clipcheck_render::dsp::{
    apply_stereo_gain, change_speed, mix_into, mono_to_stereo, normalize_gain_db, peak_db,
    resample, reverse_frames, stereo_to_mono_in_place, waveform_peaks, Limiter, LoudnessMeter,
};

const SR: u32 = 48_000;

/// Interleaved sine, `frames` frames, same signal on every channel.
fn sine(freq: f64, sr: f64, frames: usize, amp: f64, ch: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * ch);
    for n in 0..frames {
        let s = (amp * (2.0 * PI * freq * n as f64 / sr).sin()) as f32;
        for _ in 0..ch {
            out.push(s);
        }
    }
    out
}

/// Largest absolute sample in frames `from..to` (all channels).
fn peak_in(buf: &[f32], ch: usize, from: usize, to: usize) -> f32 {
    buf.get(from * ch..to * ch)
        .unwrap_or(&[])
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Largest absolute sample of the whole buffer.
fn peak_all(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn assert_close(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: expected {expected} +- {tol}, got {actual}"
    );
}

// ---------------------------------------------------------------- resample

#[test]
fn resample_equal_rate_is_identity() {
    let input = [0.1f32, 0.2, -0.3, 0.4];
    assert_eq!(resample(&input, 1, 48_000, 48_000), input.to_vec());
}

#[test]
fn resample_equal_rate_drops_partial_frame() {
    // Stereo with 5 samples: only 2 whole frames (4 samples) are returned.
    let input = [0.1f32, 0.2, 0.3, 0.4, 0.5];
    assert_eq!(
        resample(&input, 2, 48_000, 48_000),
        vec![0.1, 0.2, 0.3, 0.4]
    );
}

#[test]
fn resample_48k_to_24k_halves_frame_count() {
    let input = vec![0.25f32; 4800];
    let out = resample(&input, 1, 48_000, 24_000);
    assert_eq!(out.len(), 2400);
}

#[test]
fn resample_44100_to_48000_length_rounds() {
    // round(1000 * 48000 / 44100) = round(1088.435) = 1088.
    let input = vec![0.0f32; 1000];
    let out = resample(&input, 1, 44_100, 48_000);
    assert_eq!(out.len(), 1088);
    // Exact case: 4410 frames -> 4800 frames.
    let out2 = resample(&vec![0.0f32; 4410], 1, 44_100, 48_000);
    assert_eq!(out2.len(), 4800);
}

#[test]
fn resample_upsample_doubles_frame_count() {
    let input = vec![0.5f32; 1000];
    let out = resample(&input, 1, 24_000, 48_000);
    assert_eq!(out.len(), 2000);
}

#[test]
fn resample_stereo_length_and_channels() {
    let input = vec![0.1f32; 2000]; // 1000 stereo frames
    let out = resample(&input, 2, 48_000, 24_000);
    assert_eq!(out.len(), 1000, "500 frames x 2 channels");
    assert_eq!(out.len() % 2, 0);
}

#[test]
fn resample_zero_channels_is_empty() {
    assert!(resample(&[0.1, 0.2, 0.3], 0, 48_000, 24_000).is_empty());
}

#[test]
fn resample_zero_rate_is_empty() {
    let input = [0.1f32, 0.2, 0.3];
    assert!(resample(&input, 1, 0, 48_000).is_empty());
    assert!(resample(&input, 1, 48_000, 0).is_empty());
}

#[test]
fn resample_empty_input_is_empty() {
    assert!(resample(&[], 2, 48_000, 24_000).is_empty());
    assert!(resample(&[], 1, 44_100, 48_000).is_empty());
}

#[test]
fn resample_partial_trailing_frame_ignored() {
    // Stereo, 9 samples = 4 whole frames + 1 stray sample. 4 frames at half rate = 2 frames.
    let input = [0.1f32, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4, 0.9];
    let out = resample(&input, 2, 48_000, 24_000);
    assert_eq!(out.len(), 4);
}

#[test]
fn resample_1khz_sine_amplitude_kept_down() {
    // 48k -> 24k: a 1 kHz tone is well inside the new band and must keep its level.
    let input = sine(1_000.0, 48_000.0, 48_000, 0.5, 1);
    let out = resample(&input, 1, 48_000, 24_000);
    let p = peak_in(&out, 1, 100, out.len() - 100);
    assert!(
        (f64::from(p) - 0.5).abs() <= 0.5 * 0.05,
        "1 kHz peak after downsample: expected 0.5 +- 5 %, got {p}"
    );
}

#[test]
fn resample_1khz_sine_amplitude_kept_up() {
    // 24k -> 48k: linear interpolation keeps the sample peaks of a 1 kHz tone.
    let input = sine(1_000.0, 24_000.0, 24_000, 0.5, 1);
    let out = resample(&input, 1, 24_000, 48_000);
    let p = peak_in(&out, 1, 200, out.len() - 200);
    assert!(
        (f64::from(p) - 0.5).abs() <= 0.5 * 0.05,
        "1 kHz peak after upsample: expected 0.5 +- 5 %, got {p}"
    );
}

#[test]
fn resample_downsample_suppresses_20khz_tone() {
    // 20 kHz is above the new Nyquist (12 kHz). The low-pass must cut it by more than 20 dB,
    // i.e. the output peak must be below 0.1 for a 1.0 input tone.
    let input = sine(20_000.0, 48_000.0, 48_000, 1.0, 1);
    let out = resample(&input, 1, 48_000, 24_000);
    let p = peak_in(&out, 1, 200, out.len() - 200);
    assert!(p < 0.1, "20 kHz tone not suppressed by > 20 dB: peak {p}");
}

#[test]
fn resample_dc_level_preserved() {
    let input = vec![0.5f32; 4800];
    let out = resample(&input, 1, 48_000, 24_000);
    for &s in out.get(200..).unwrap_or(&[]) {
        assert!((s - 0.5).abs() < 1e-3, "DC level drifted: {s}");
    }
}

#[test]
fn resample_stereo_channels_independent() {
    // Left carries a tone, right is silent. Right must stay silent after resampling.
    let tone = sine(1_000.0, 48_000.0, 4800, 0.5, 1);
    let mut input = Vec::with_capacity(tone.len() * 2);
    for s in &tone {
        input.push(*s);
        input.push(0.0);
    }
    let out = resample(&input, 2, 48_000, 24_000);
    let right_peak = out
        .iter()
        .skip(1)
        .step_by(2)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let left_peak = out.iter().step_by(2).fold(0.0f32, |m, s| m.max(s.abs()));
    assert_eq!(right_peak, 0.0, "silent channel leaked");
    assert!(left_peak > 0.4, "active channel lost: {left_peak}");
}

// ---------------------------------------------------------------- change_speed

#[test]
fn change_speed_2x_halves_length() {
    let input = vec![0.1f32; 1000];
    assert_eq!(change_speed(&input, 1, 2.0).len(), 500);
}

#[test]
fn change_speed_half_doubles_length() {
    let input = vec![0.1f32; 1000];
    assert_eq!(change_speed(&input, 1, 0.5).len(), 2000);
}

#[test]
fn change_speed_1x_is_identity() {
    let input: Vec<f32> = (0..50).map(|i| i as f32 * 0.01).collect();
    assert_eq!(change_speed(&input, 1, 1.0), input);
}

#[test]
fn change_speed_nonpositive_or_nonfinite_is_empty() {
    let input = vec![0.1f32; 100];
    assert!(change_speed(&input, 1, 0.0).is_empty());
    assert!(change_speed(&input, 1, -1.0).is_empty());
    assert!(change_speed(&input, 1, f64::NAN).is_empty());
    assert!(change_speed(&input, 1, f64::INFINITY).is_empty());
}

#[test]
fn change_speed_zero_channels_is_empty() {
    assert!(change_speed(&[0.1, 0.2], 0, 2.0).is_empty());
}

#[test]
fn change_speed_stereo_length() {
    let input = vec![0.1f32; 200]; // 100 stereo frames
    let out = change_speed(&input, 2, 2.0);
    assert_eq!(out.len(), 100, "50 frames x 2 channels");
}

// ---------------------------------------------------------------- reverse_frames

#[test]
fn reverse_stereo_frame_order() {
    let input = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
    assert_eq!(
        reverse_frames(&input, 2),
        vec![5.0, 6.0, 3.0, 4.0, 1.0, 2.0],
        "frame pairs must stay together"
    );
}

#[test]
fn reverse_mono() {
    assert_eq!(reverse_frames(&[1.0, 2.0, 3.0], 1), vec![3.0, 2.0, 1.0]);
}

#[test]
fn reverse_twice_is_identity() {
    let input: Vec<f32> = (0..16).map(|i| i as f32).collect();
    let once = reverse_frames(&input, 2);
    assert_eq!(reverse_frames(&once, 2), input);
}

#[test]
fn reverse_zero_channels_is_empty() {
    assert!(reverse_frames(&[1.0, 2.0], 0).is_empty());
}

// ---------------------------------------------------------------- mix / gain / channels

#[test]
fn mix_into_adds_scaled_source() {
    let mut dst = [1.0f32, 1.0, 1.0];
    mix_into(&mut dst, &[0.5, -0.5, 0.25], 2.0);
    assert_eq!(dst, [2.0, 0.0, 1.5]);
}

#[test]
fn mix_into_stops_at_shorter_length() {
    let mut dst = [0.0f32; 4];
    mix_into(&mut dst, &[1.0, 1.0], 1.0);
    assert_eq!(dst, [1.0, 1.0, 0.0, 0.0]);
}

#[test]
fn apply_stereo_gain_per_channel() {
    let mut buf = [1.0f32, 1.0, 2.0, 2.0];
    apply_stereo_gain(&mut buf, 0.5, 0.25);
    assert_eq!(buf, [0.5, 0.25, 1.0, 0.5]);
}

#[test]
fn stereo_to_mono_averages_pairs() {
    let mut buf = [1.0f32, 0.0, -1.0, 0.5];
    stereo_to_mono_in_place(&mut buf);
    assert_eq!(buf, [0.5, 0.5, -0.25, -0.25]);
}

#[test]
fn mono_to_stereo_duplicates() {
    assert_eq!(mono_to_stereo(&[0.1, 0.2]), vec![0.1, 0.1, 0.2, 0.2]);
}

// ---------------------------------------------------------------- peak_db

#[test]
fn peak_db_full_scale_is_zero() {
    assert_close(peak_db(&[1.0, -0.5]), 0.0, 1e-9, "full scale");
}

#[test]
fn peak_db_empty_is_minus_120() {
    assert_eq!(peak_db(&[]), -120.0);
}

#[test]
fn peak_db_silence_is_minus_120() {
    assert_eq!(peak_db(&[0.0; 8]), -120.0);
}

#[test]
fn peak_db_half_is_minus_6_02() {
    assert_close(peak_db(&[0.5]), -6.0206, 1e-3, "half scale");
}

// ---------------------------------------------------------------- waveform_peaks

#[test]
fn waveform_peaks_bucket_count() {
    let input = vec![0.3f32; 100];
    assert_eq!(waveform_peaks(&input, 1, 10).len(), 10);
}

#[test]
fn waveform_peaks_max_abs_per_bucket() {
    let input = [0.1f32, -0.9, 0.2, 0.3, 0.0, 0.0, -0.5, 0.4];
    let out = waveform_peaks(&input, 1, 2);
    assert_eq!(out, vec![0.9, 0.5]);
}

#[test]
fn waveform_peaks_empty_input_zeros() {
    assert_eq!(waveform_peaks(&[], 2, 4), vec![0.0; 4]);
}

#[test]
fn waveform_peaks_zero_buckets_or_channels_empty() {
    assert!(waveform_peaks(&[0.5, 0.5], 1, 0).is_empty());
    assert!(waveform_peaks(&[0.5, 0.5], 0, 2).is_empty());
}

#[test]
fn waveform_peaks_stereo_uses_all_channels() {
    // Two stereo frames in one bucket; the peak sits in the right channel of frame 2.
    let input = [0.0f32, 0.2, 0.7, -0.1];
    assert_eq!(waveform_peaks(&input, 2, 1), vec![0.7]);
}

#[test]
fn waveform_peaks_clamped_to_one() {
    assert_eq!(waveform_peaks(&[2.0, -3.0], 1, 1), vec![1.0]);
}

#[test]
fn waveform_peaks_more_buckets_than_frames() {
    // 2 frames into 4 buckets: frame 0 -> bucket 0, frame 1 -> bucket 2.
    let out = waveform_peaks(&[0.5, 0.25], 1, 4);
    assert_eq!(out, vec![0.5, 0.0, 0.25, 0.0]);
}

// ---------------------------------------------------------------- Limiter

#[test]
fn limiter_caps_plus6db_sine_at_ceiling() {
    // +6 dB full-scale stereo tone, ceiling -1 dBFS. Output must stay within ceiling + 0.1 dB.
    let amp = 10f64.powf(6.0 / 20.0);
    let input = sine(997.0, 48_000.0, 24_000, amp, 2);
    let mut lim = Limiter::new(-1.0, SR, 2);
    let mut buf = input.clone();
    lim.process(&mut buf);
    buf.extend(lim.flush());
    let ceiling_plus_tol = 10f64.powf((-1.0 + 0.1) / 20.0) as f32;
    let peak = peak_all(&buf);
    assert!(
        peak <= ceiling_plus_tol,
        "limiter output peak {peak} exceeds ceiling + 0.1 dB ({ceiling_plus_tol})"
    );
    assert!(
        peak > 0.85,
        "limiter did not engage near the ceiling: {peak}"
    );
}

#[test]
fn limiter_quiet_signal_passes_delayed_unchanged() {
    // Peak 0.1 is far below the -1 dBFS ceiling: no gain change, only the 5 ms delay (240 frames).
    let input = sine(997.0, 48_000.0, 2000, 0.1, 2);
    let mut lim = Limiter::new(-1.0, SR, 2);
    let mut buf = input.clone();
    lim.process(&mut buf);
    buf.extend(lim.flush());
    let look = 240usize;
    for (i, &s) in input.iter().enumerate() {
        let out = buf.get(i + look * 2).copied().unwrap_or(f32::NAN);
        assert!(
            (out - s).abs() < 1e-5,
            "sample {i}: expected delayed {s}, got {out}"
        );
    }
}

#[test]
fn limiter_output_length_is_input_plus_lookahead() {
    let input = vec![0.1f32; 2000]; // 1000 stereo frames
    let mut lim = Limiter::new(-1.0, SR, 2);
    let mut buf = input.clone();
    lim.process(&mut buf);
    let tail = lim.flush();
    assert_eq!(tail.len(), 240 * 2, "tail is one 5 ms look-ahead at 48 kHz");
    assert_eq!(buf.len() + tail.len(), input.len() + 240 * 2);
}

#[test]
fn limiter_block_size_does_not_change_output() {
    let input = sine(1_000.0, 48_000.0, 4800, 1.5, 2);

    let mut one = Limiter::new(-1.0, SR, 2);
    let mut a = input.clone();
    one.process(&mut a);
    a.extend(one.flush());

    let mut chunked = Limiter::new(-1.0, SR, 2);
    let mut b = Vec::with_capacity(input.len());
    for chunk in input.chunks(74) {
        let mut c = chunk.to_vec();
        chunked.process(&mut c);
        b.extend(c);
    }
    b.extend(chunked.flush());

    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert!(
            (x - y).abs() < 1e-6,
            "block size changed output: {x} vs {y}"
        );
    }
}

#[test]
fn limiter_flush_resets_state() {
    let input = sine(440.0, 48_000.0, 1000, 0.2, 2);
    let mut lim = Limiter::new(-1.0, SR, 2);
    let mut first = input.clone();
    lim.process(&mut first);
    first.extend(lim.flush());

    let mut second = input.clone();
    lim.process(&mut second);
    second.extend(lim.flush());

    assert_eq!(
        first, second,
        "flush must reset the limiter for a new stream"
    );
}

// ---------------------------------------------------------------- LoudnessMeter

#[test]
fn loudness_mono_minus20dbfs_is_minus23() {
    // Mono 997 Hz sine, peak -20 dBFS. BS.1770: full-scale mono 997 Hz is about -3.01 LUFS,
    // so -20 dB gives about -23.0 LUFS.
    let amp = 0.1;
    let data = sine(997.0, 48_000.0, 5 * 48_000, amp, 1);
    let mut m = LoudnessMeter::new(SR, 1);
    m.push(&data);
    let lufs = m.integrated_lufs().expect("loudness present");
    assert_close(lufs, -23.0, 0.5, "mono -20 dBFS loudness");
}

#[test]
fn loudness_stereo_minus20dbfs_is_minus20() {
    // Stereo with identical channels doubles the power (+3.01 dB) versus mono, so the
    // BS.1770 value for a -20 dBFS peak per channel is about -20.0 LUFS (not -23.0).
    let data = sine(997.0, 48_000.0, 5 * 48_000, 0.1, 2);
    let mut m = LoudnessMeter::new(SR, 2);
    m.push(&data);
    let lufs = m.integrated_lufs().expect("loudness present");
    assert_close(lufs, -20.0, 0.5, "stereo -20 dBFS loudness");
}

#[test]
fn loudness_silence_is_none() {
    let mut m = LoudnessMeter::new(SR, 2);
    m.push(&vec![0.0f32; 2 * 5 * 48_000]);
    assert_eq!(m.integrated_lufs(), None);
}

#[test]
fn loudness_too_short_is_none() {
    // 0.3 s is shorter than one 400 ms gating block.
    let mut m = LoudnessMeter::new(SR, 1);
    m.push(&sine(997.0, 48_000.0, 14_400, 0.5, 1));
    assert_eq!(m.integrated_lufs(), None);
}

#[test]
fn loudness_absolute_gate_quiet_is_none() {
    // Amplitude 1e-5 (about -100 dBFS) sits far below the -70 LUFS absolute gate.
    let mut m = LoudnessMeter::new(SR, 1);
    m.push(&sine(997.0, 48_000.0, 5 * 48_000, 1e-5, 1));
    assert_eq!(m.integrated_lufs(), None);
}

#[test]
fn loudness_doubling_amplitude_adds_6db() {
    let mut quiet = LoudnessMeter::new(SR, 1);
    quiet.push(&sine(997.0, 48_000.0, 5 * 48_000, 0.05, 1));
    let mut loud = LoudnessMeter::new(SR, 1);
    loud.push(&sine(997.0, 48_000.0, 5 * 48_000, 0.1, 1));
    let diff = loud.integrated_lufs().expect("loud") - quiet.integrated_lufs().expect("quiet");
    assert_close(diff, 6.02, 0.1, "amplitude doubling");
}

#[test]
fn loudness_streaming_matches_single_push() {
    let data = sine(997.0, 48_000.0, 2 * 48_000, 0.1, 2);
    let mut whole = LoudnessMeter::new(SR, 2);
    whole.push(&data);

    let mut streamed = LoudnessMeter::new(SR, 2);
    let sizes = [1usize, 7, 480, 1023, 3, 999];
    let total_frames = data.len() / 2;
    let mut frame = 0usize;
    let mut k = 0usize;
    while frame < total_frames {
        let n = sizes[k % sizes.len()].min(total_frames - frame);
        streamed.push(&data[frame * 2..(frame + n) * 2]);
        frame += n;
        k += 1;
    }

    let a = whole.integrated_lufs().expect("whole");
    let b = streamed.integrated_lufs().expect("streamed");
    assert_close(a, b, 0.05, "streamed vs single push");
}

// ---------------------------------------------------------------- normalize_gain_db

#[test]
fn normalize_gain_basic() {
    assert_close(normalize_gain_db(-20.0, -23.0), -3.0, 1e-9, "-20 to -23");
    assert_close(normalize_gain_db(-26.0, -23.0), 3.0, 1e-9, "-26 to -23");
}

#[test]
fn normalize_gain_clamped_plus_30() {
    assert_eq!(normalize_gain_db(-60.0, -23.0), 30.0);
}

#[test]
fn normalize_gain_clamped_minus_30() {
    assert_eq!(normalize_gain_db(10.0, -23.0), -30.0);
}

#[test]
fn normalize_gain_nonfinite_is_zero() {
    assert_eq!(normalize_gain_db(f64::NEG_INFINITY, -23.0), 0.0);
    assert_eq!(normalize_gain_db(f64::NAN, -23.0), 0.0);
}
