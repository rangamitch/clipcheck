import { describe, it, expect, vi } from 'vitest';
import { chunkPages, shouldRenderPage, refetchTarget, wantsMorePages, createLatestLoader } from './gallerywindow';

function deferred<T>() {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

describe('chunkPages', () => {
	it('issue_80 returns [] for empty input', () => {
		expect(chunkPages([], 3)).toEqual([]);
		expect(chunkPages([], 0)).toEqual([]);
	});

	it('issue_80 splits an exact multiple into full chunks', () => {
		expect(chunkPages([1, 2, 3, 4, 5, 6], 3)).toEqual([
			[1, 2, 3],
			[4, 5, 6]
		]);
	});

	it('issue_80 keeps the remainder as a short last chunk', () => {
		expect(chunkPages([1, 2, 3, 4, 5, 6, 7], 3)).toEqual([[1, 2, 3], [4, 5, 6], [7]]);
	});

	it('issue_80 size 1 yields one chunk per item', () => {
		expect(chunkPages(['a', 'b', 'c'], 1)).toEqual([['a'], ['b'], ['c']]);
	});

	it('issue_80 size larger than length yields a single chunk', () => {
		expect(chunkPages([1, 2], 60)).toEqual([[1, 2]]);
	});

	it('issue_80 size 0 is treated as 1', () => {
		expect(chunkPages([1, 2, 3], 0)).toEqual([[1], [2], [3]]);
	});

	it('issue_80 negative size (-3) is treated as 1', () => {
		expect(chunkPages([1, 2, 3], -3)).toEqual([[1], [2], [3]]);
	});

	it('issue_80 does not mutate the input array', () => {
		const input = [1, 2, 3, 4, 5];
		const copy = [...input];
		chunkPages(input, 2);
		expect(input).toEqual(copy);
		expect(input.length).toBe(5);
	});
});

describe('shouldRenderPage', () => {
	it('issue_80 visible and never measured -> true', () => {
		expect(shouldRenderPage(0, new Set([0]), new Map())).toBe(true);
	});

	it('issue_80 visible and measured -> true', () => {
		expect(shouldRenderPage(2, new Set([2]), new Map([[2, 900]]))).toBe(true);
	});

	it('issue_80 not visible and never measured -> true', () => {
		expect(shouldRenderPage(5, new Set([0, 1]), new Map())).toBe(true);
	});

	it('issue_80 not visible and measured -> false', () => {
		expect(shouldRenderPage(5, new Set([0, 1]), new Map([[5, 1200]]))).toBe(false);
	});

	it('issue_80 empty visible set with a measured page -> false', () => {
		expect(shouldRenderPage(3, new Set(), new Map([[3, 400]]))).toBe(false);
	});

	it('issue_80 measurement for a different index does not affect this page', () => {
		expect(shouldRenderPage(4, new Set(), new Map([[3, 400]]))).toBe(true);
	});
});

describe('refetchTarget', () => {
	it('issue_47 three loaded pages of 60 -> target 180', () => {
		expect(refetchTarget(180, 60)).toBe(180);
	});

	it('issue_47 fewer than a page loaded -> target is one page (60)', () => {
		expect(refetchTarget(30, 60)).toBe(60);
	});

	it('issue_47 nothing loaded -> target is one page', () => {
		expect(refetchTarget(0, 60)).toBe(60);
	});

	it('issue_47 exactly one page loaded -> 60', () => {
		expect(refetchTarget(60, 60)).toBe(60);
	});
});

describe('wantsMorePages', () => {
	it('issue_47 false when cursor is null even if under target', () => {
		expect(wantsMorePages(0, 180, null)).toBe(false);
	});

	it('issue_47 true when under target and cursor present', () => {
		expect(wantsMorePages(60, 180, 'c2')).toBe(true);
	});

	it('issue_47 false when collected equals target', () => {
		expect(wantsMorePages(180, 180, 'c4')).toBe(false);
	});

	it('issue_47 false when collected exceeds target', () => {
		expect(wantsMorePages(240, 180, 'c5')).toBe(false);
	});
});

describe('refetch loop simulation', () => {
	type Page = { items: number[]; next: string | null };

	function runRefetch(pages: Page[], loadedCount: number, pageSize: number): number {
		const target = refetchTarget(loadedCount, pageSize);
		let collected = 0;
		let fetches = 0;
		let cursorIdx = 0;
		let cursor: string | null = 'start';
		while (true) {
			const page = pages[cursorIdx];
			fetches++;
			collected += page.items.length;
			cursor = page.next;
			cursorIdx++;
			if (!wantsMorePages(collected, target, cursor)) break;
		}
		return fetches;
	}

	const pageOf = (n: number, last: boolean): Page => ({
		items: Array.from({ length: 60 }, (_, i) => n * 60 + i),
		next: last ? null : `c${n + 1}`
	});

	it('issue_47 refresh after 3 loaded pages fetches 3 pages then stops at target 180', () => {
		const pages = [0, 1, 2, 3, 4].map((n) => pageOf(n, n === 4));
		expect(runRefetch(pages, 180, 60)).toBe(3);
	});

	it('issue_47 refresh stops early when cursor becomes null', () => {
		const pages = [pageOf(0, false), pageOf(1, true)];
		expect(runRefetch(pages, 180, 60)).toBe(2);
	});

	it('issue_47 refresh with one loaded page fetches one page', () => {
		const pages = [pageOf(0, false), pageOf(1, false)];
		expect(runRefetch(pages, 60, 60)).toBe(1);
	});
});

describe('createLatestLoader', () => {
	it('issue_62 applies the value on success', async () => {
		const apply = vi.fn();
		const fetchFn = vi.fn(async () => 42);
		const load = createLatestLoader(fetchFn, apply);
		await load();
		expect(fetchFn).toHaveBeenCalledTimes(1);
		expect(apply).toHaveBeenCalledTimes(1);
		expect(apply).toHaveBeenCalledWith(42);
	});

	it('issue_62 sequential successful calls each apply', async () => {
		const apply = vi.fn();
		let n = 0;
		const load = createLatestLoader(async () => ++n, apply);
		await load();
		await load();
		expect(apply.mock.calls).toEqual([[1], [2]]);
	});

	it('issue_62 failure is swallowed, apply not called, previous value kept', async () => {
		const apply = vi.fn();
		const fetchFn = vi
			.fn<() => Promise<number>>()
			.mockResolvedValueOnce(1)
			.mockRejectedValueOnce(new Error('boom'));
		const load = createLatestLoader(fetchFn, apply);
		await load();
		await expect(load()).resolves.toBeUndefined();
		expect(apply.mock.calls).toEqual([[1]]);
	});

	it('issue_62 a rejection as the only call never throws and never applies', async () => {
		const apply = vi.fn();
		const load = createLatestLoader(() => Promise.reject(new Error('down')), apply);
		await expect(load()).resolves.toBeUndefined();
		expect(apply).not.toHaveBeenCalled();
	});

	it('issue_62 synchronous throw from fetchFn does not propagate', async () => {
		const apply = vi.fn();
		const load = createLatestLoader((): Promise<number> => {
			throw new Error('sync');
		}, apply);
		await expect(load()).resolves.toBeUndefined();
		expect(apply).not.toHaveBeenCalled();
	});

	it('issue_62 out-of-order: first call resolving after second applies only the second', async () => {
		const ds = [deferred<string>(), deferred<string>()];
		let i = 0;
		const fetchFn = () => ds[i++].promise;
		const apply = vi.fn();
		const load = createLatestLoader(fetchFn, apply);
		const p1 = load();
		const p2 = load();
		ds[1].resolve('second');
		await p2;
		ds[0].resolve('first');
		await p1;
		expect(apply.mock.calls).toEqual([['second']]);
	});

	it('issue_62 out-of-order: stale first call rejecting after newer success does not throw or apply', async () => {
		const ds = [deferred<string>(), deferred<string>()];
		let i = 0;
		const apply = vi.fn();
		const load = createLatestLoader(() => ds[i++].promise, apply);
		const p1 = load();
		const p2 = load();
		ds[1].resolve('fresh');
		await p2;
		ds[0].reject(new Error('stale failure'));
		await expect(p1).resolves.toBeUndefined();
		expect(apply.mock.calls).toEqual([['fresh']]);
	});

	it('issue_62 concurrent calls: only the latest started call is applied', async () => {
		let n = 0;
		const apply = vi.fn();
		const fetchFn = vi.fn(async () => ++n);
		const load = createLatestLoader(fetchFn, apply);
		await Promise.all([load(), load(), load()]);
		expect(fetchFn).toHaveBeenCalledTimes(3);
		expect(apply.mock.calls).toEqual([[3]]);
	});
});
