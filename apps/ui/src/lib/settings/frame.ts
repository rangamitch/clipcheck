/**
 * Coalesces rapid calls, such as slider drag events, into at most one `apply` per animation
 * frame. The latest value wins, so the draft ends up with the same value as an unthrottled
 * drag, and each frame writes once instead of once per input event.
 *
 * Where `requestAnimationFrame` is missing (tests, non-browser hosts) the value is applied at once.
 */
export function frameCoalesced<T>(apply: (value: T) => void): (value: T) => void {
  let pending: { value: T } | null = null;
  let scheduled = false;

  return (value: T) => {
    if (typeof requestAnimationFrame !== 'function') {
      apply(value);
      return;
    }
    pending = { value };
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      const next = pending;
      pending = null;
      if (next) apply(next.value);
    });
  };
}
