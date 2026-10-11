/**
 * Pure helpers for the gallery's paging and windowed rendering. No Svelte, no I/O, so they can
 * be tested directly.
 */

/**
 * Splits `items` into consecutive chunks of `size`. The last chunk may be shorter. A `size`
 * below 1 is treated as 1. Empty input gives an empty array.
 */
export function chunkPages<T>(items: readonly T[], size: number): T[][] {
  const step = Math.max(1, Math.floor(size) || 1);
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += step) {
    out.push(items.slice(i, i + step));
  }
  return out;
}

/**
 * Whether page `index` should render its cards. A page renders when it is in `visible`, and
 * also when it has never been measured (no entry in `heights`), so it renders once and can be
 * measured. Only a page that is off screen and has a recorded height is replaced by a spacer.
 */
export function shouldRenderPage(
  index: number,
  visible: ReadonlySet<number>,
  heights: ReadonlyMap<number, number>,
): boolean {
  if (visible.has(index)) return true;
  return !heights.has(index);
}

/**
 * How many items a refetch of the list should collect: the number now loaded, but at least one
 * page, so the list keeps its size across a refresh.
 */
export function refetchTarget(loadedCount: number, pageSize: number): number {
  return Math.max(loadedCount, pageSize);
}

/**
 * Whether a refetch should request another page: the listing has a next cursor and fewer
 * items than `target` have been collected so far.
 */
export function wantsMorePages(
  collectedCount: number,
  target: number,
  nextCursor: string | null,
): boolean {
  return nextCursor !== null && collectedCount < target;
}

/**
 * Builds a loader where each call starts a fetch. Only the newest call's result is applied:
 * when a call resolves after a newer call has started, its value is dropped. A rejected fetch
 * is swallowed and `apply` is not called, so the previous value stays. The returned function
 * never rejects.
 */
export function createLatestLoader<T>(
  fetchFn: () => Promise<T>,
  apply: (value: T) => void,
): () => Promise<void> {
  let seq = 0;
  return async () => {
    const mine = ++seq;
    let value: T;
    try {
      value = await fetchFn();
    } catch {
      return;
    }
    if (mine === seq) apply(value);
  };
}
