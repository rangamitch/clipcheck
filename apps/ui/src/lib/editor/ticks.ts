// Tick maths for the video editor (pure, no DOM).
//
// One Tick is 1 / 254,016,000,000 s, the same unit as the Rust `timeline::time::Tick`. Every tick
// value stays below 2^53 so it is an exact JavaScript number. Frame and tick conversions use BigInt
// internally so that the results are exact, not float approximations.

import type { Clip, Rate, Tick } from './types';

/** Ticks per second (matches `timeline::time::TICKS_PER_SECOND`). */
export const TICKS_PER_SECOND = 254_016_000_000;

/** Largest tick value the model accepts (2^53 - 1). */
export const MAX_TICK = Number.MAX_SAFE_INTEGER;

/** Longest project timeline: 4 hours. */
export const MAX_PROJECT_TICKS = 4 * 3600 * TICKS_PER_SECOND;

/** Ticks per millisecond (an integer: 254,016,000). */
const TICKS_PER_MS = TICKS_PER_SECOND / 1000;

/** Used when a Rate is missing or invalid, so formatting still produces something sensible. */
const FALLBACK_RATE: Rate = { num: 30, den: 1 };

const TPS_BIG = BigInt(TICKS_PER_SECOND);

/** True when `rate` has a usable numerator and denominator. */
function validRate(rate: Rate): boolean {
	return Number.isInteger(rate.num) && Number.isInteger(rate.den) && rate.num > 0 && rate.den > 0;
}

/** Clamps a finite number to the tick range and rounds it to an integer. Non-finite gives 0. */
function toTick(v: number): Tick {
	if (!Number.isFinite(v)) return 0;
	const r = Math.round(v);
	return Math.min(MAX_TICK, Math.max(-MAX_TICK, r));
}

/** Floor division for BigInt values (`b` positive). */
function floorDivBig(a: bigint, b: bigint): bigint {
	const q = a / b;
	return a % b !== 0n && a < 0n ? q - 1n : q;
}

/** Nominal integer frame rate used for the frame field of timecodes. */
function nominalFps(rate: Rate): number {
	const r = validRate(rate) ? rate : FALLBACK_RATE;
	return Math.max(1, Math.round(r.num / r.den));
}

/** Frame index containing tick `t` at `rate` (floor; non-positive `t` gives 0). */
function frameIndexOf(t: Tick, rate: Rate): bigint {
	const r = validRate(rate) ? rate : FALLBACK_RATE;
	const tb = BigInt(toTick(t));
	if (tb <= 0n) return 0n;
	return (tb * BigInt(r.num)) / (BigInt(r.den) * TPS_BIG);
}

/** Tick at the start of frame `n` at `rate` (rounded to the nearest tick). */
function frameStartOf(n: bigint, rate: Rate): Tick {
	const r = validRate(rate) ? rate : FALLBACK_RATE;
	const num = BigInt(r.num);
	const den = BigInt(r.den);
	// round(n * den * TPS / num), computed as floor((2 * n * den * TPS + num) / (2 * num)).
	const numer = 2n * n * den * TPS_BIG + num;
	const q = floorDivBig(numer, 2n * num);
	return toTick(Number(q));
}

/** Seconds (float) to the nearest tick. NaN and infinities give 0. */
export function secToTick(s: number): Tick {
	if (!Number.isFinite(s)) return 0;
	return toTick(s * TICKS_PER_SECOND);
}

/** Tick to seconds (float, for display only). Non-finite input gives 0. */
export function tickToSec(t: Tick): number {
	if (!Number.isFinite(t)) return 0;
	return t / TICKS_PER_SECOND;
}

/** Milliseconds to the nearest tick. NaN and infinities give 0. */
export function msToTick(ms: number): Tick {
	if (!Number.isFinite(ms)) return 0;
	return toTick(ms * TICKS_PER_MS);
}

/** Tick to whole milliseconds, rounded to nearest. Non-finite input gives 0. */
export function tickToMs(t: Tick): number {
	if (!Number.isFinite(t)) return 0;
	return Math.round(t / TICKS_PER_MS);
}

/** Duration of one frame at `rate`, in ticks (`round(TPS * den / num)`). Invalid rate gives 0. */
export function frameDurationTicks(rate: Rate): Tick {
	if (!validRate(rate)) return 0;
	return toTick((TICKS_PER_SECOND * rate.den) / rate.num);
}

/** Snaps `t` to the nearest frame boundary at `rate`. Non-positive or non-finite `t` gives 0. */
export function snapToFrame(t: Tick, rate: Rate): Tick {
	if (!Number.isFinite(t) || t <= 0) return 0;
	const r = validRate(rate) ? rate : FALLBACK_RATE;
	const tb = BigInt(toTick(t));
	const num = BigInt(r.num);
	const den = BigInt(r.den);
	// nearest frame index: floor((2 * t * num + den * TPS) / (2 * den * TPS)).
	const d = den * TPS_BIG;
	const frame = floorDivBig(2n * tb * num + d, 2n * d);
	return frameStartOf(frame, r);
}

/** Pads a non-negative integer to at least two digits. */
function pad2(n: number): string {
	return n < 10 ? `0${n}` : String(n);
}

/**
 * Timecode `HH:MM:SS:FF` for tick `t` at `rate`. The frame field counts frames within the second
 * using the nominal integer rate (`round(num / den)`). Negative and non-finite times show as 0.
 */
export function formatTimecode(t: Tick, rate: Rate): string {
	const fps = nominalFps(rate);
	const frame = Number(frameIndexOf(t, rate));
	const ff = frame % fps;
	const totalSec = Math.floor(frame / fps);
	const hh = Math.floor(totalSec / 3600);
	const mm = Math.floor((totalSec % 3600) / 60);
	const ss = totalSec % 60;
	return `${pad2(hh)}:${pad2(mm)}:${pad2(ss)}:${pad2(ff)}`;
}

/**
 * Duration label: `M:SS` below one hour, otherwise `H:MM:SS`. Whole seconds, floored. Negative and
 * non-finite values show as 0:00.
 */
export function formatDuration(t: Tick): string {
	const tb = Number.isFinite(t) ? BigInt(toTick(t)) : 0n;
	const secs = tb <= 0n ? 0 : Number(floorDivBig(tb, TPS_BIG));
	const h = Math.floor(secs / 3600);
	const m = Math.floor((secs % 3600) / 60);
	const s = secs % 60;
	if (h > 0) return `${h}:${pad2(m)}:${pad2(s)}`;
	return `${m}:${pad2(s)}`;
}

const RE_INT = /^\d+$/;
const RE_DECIMAL = /^\d+(\.\d+)?$/;

/** Parses a non-negative integer part, or returns null. */
function intPart(s: string): number | null {
	if (!RE_INT.test(s)) return null;
	const n = Number(s);
	return Number.isSafeInteger(n) ? n : null;
}

/**
 * Parses user-typed time text into a tick. Accepted forms:
 * - `HH:MM:SS:FF`, `MM:SS:FF`, `SS:FF` (frame count at `rate`; MM and SS must be below 60, FF below
 *   the nominal rate)
 * - `H:MM:SS.s`, `M:SS.s`, `SS.s`, or with a `.` anywhere in the last part (decimal seconds)
 * - plain whole seconds such as `12`
 * Returns null for empty, negative, NaN or otherwise malformed input.
 */
export function parseTimecode(text: string, rate: Rate): Tick | null {
	const s = text.trim();
	if (s.length === 0) return null;
	const parts = s.split(':');
	if (parts.length > 4) return null;

	const last = parts[parts.length - 1];

	// Decimal-seconds form: `12.5`, `1:23.5`, `1:02:03.25`. Only the last part may have a fraction.
	if (last.includes('.') || parts.length === 1) {
		if (parts.length === 1 && !RE_DECIMAL.test(s)) return null;
		if (!RE_DECIMAL.test(last)) return null;
		const lead = parts.slice(0, -1);
		let total = 0;
		if (lead.length === 0) {
			total = Number(last);
		} else {
			// Leading parts are hours (if three parts), then minutes; minutes and seconds below 60.
			const nums: number[] = [];
			for (const p of lead) {
				const n = intPart(p);
				if (n === null) return null;
				nums.push(n);
			}
			const sec = Number(last);
			if (!Number.isFinite(sec) || sec >= 60) return null;
			if (nums.length === 1) {
				total = nums[0] * 60 + sec;
			} else if (nums.length === 2) {
				if (nums[1] >= 60) return null;
				total = (nums[0] * 3600) + nums[1] * 60 + sec;
			} else {
				return null;
			}
		}
		if (!Number.isFinite(total) || total < 0) return null;
		return secToTick(total);
	}

	// Frame-count forms `SS:FF`, `MM:SS:FF`, `HH:MM:SS:FF`.
	const fps = nominalFps(rate);
	const nums: number[] = [];
	for (const p of parts) {
		const n = intPart(p);
		if (n === null) return null;
		nums.push(n);
	}
	const ff = nums[nums.length - 1];
	if (ff >= fps) return null;
	let hh = 0;
	let mm = 0;
	let ss: number;
	if (nums.length === 2) {
		ss = nums[0];
	} else if (nums.length === 3) {
		mm = nums[0];
		ss = nums[1];
		if (mm >= 60) return null;
	} else {
		hh = nums[0];
		mm = nums[1];
		ss = nums[2];
		if (mm >= 60) return null;
	}
	if (ss >= 60) return null;
	const frame = ((hh * 60 + mm) * 60 + ss) * fps + ff;
	return frameStartOf(BigInt(frame), rate);
}

/** End of a clip on the timeline (`start + dur`). Non-finite values give 0. */
export function clipEnd(clip: Clip): Tick {
	return toTick(clip.start + clip.dur);
}

/** Length consumed from the source: `round(dur * speed)`, clamped to 0 and the tick range. */
export function srcLen(clip: Clip): Tick {
	const v = Math.round(clip.dur * clip.speed);
	if (!Number.isFinite(v) || v <= 0) return 0;
	return Math.min(MAX_TICK, v);
}

/** Clamps `t` into `lo..hi`. Returns `lo` when `hi < lo` or `t` is non-finite. */
export function clampTick(t: Tick, lo: Tick, hi: Tick): Tick {
	if (hi < lo) return lo;
	if (!Number.isFinite(t)) return lo;
	return Math.min(hi, Math.max(lo, Math.round(t)));
}
