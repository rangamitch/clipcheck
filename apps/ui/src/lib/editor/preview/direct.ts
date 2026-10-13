// Direct-video mode decisions of the preview engine. When a segment is one plain video layer, the
// playing <video> element itself is shown (visible, unoccluded) instead of copying frames through
// the canvas. These pure helpers decide when to enter and leave; engine.ts owns the DOM work.

/** Facts that decide whether direct mode may start. */
export interface DirectState {
  /** Kill switch and host availability (engine constant and an attached direct host). */
  enabled: boolean;
  playing: boolean;
  /** Signed timeline rate. */
  rate: number;
  scrubbing: boolean;
  /** A transform override (drag) is active. */
  override: boolean;
  /** The segment has exactly one layer, a plain eligible video (see directLayout). */
  eligible: boolean;
  failed: boolean;
  elementPlaying: boolean;
  /** HTMLMediaElement.readyState of the element. */
  readyState: number;
  seeking: boolean;
  /** Frames presented since the element was positioned for this segment. */
  framesSinceArm: number;
  /** The last canvas paint had every source frame. */
  canvasPainted: boolean;
}

/** True when the state allows direct mode to continue. */
export function directAllowed(s: DirectState): boolean {
  return (
    s.enabled &&
    s.playing &&
    s.rate === 1 &&
    !s.scrubbing &&
    !s.override &&
    s.eligible &&
    !s.failed &&
    s.elementPlaying &&
    s.readyState >= 2
  );
}

/** Direct mode starts only when allowed and the element has already presented a frame (no black flash). */
export function shouldEnterDirect(s: DirectState): boolean {
  return directAllowed(s) && !s.seeking && s.framesSinceArm >= 1 && s.canvasPainted;
}

/** Direct mode ends as soon as it is no longer allowed or the element is seeking back to black. */
export function shouldLeaveDirect(s: DirectState): boolean {
  return !directAllowed(s);
}
