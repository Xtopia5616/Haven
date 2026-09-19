import { describe, expect, it, vi } from 'vitest';
import { createSessionRefreshScheduler } from './sessionRefresh.ts';

describe('createSessionRefreshScheduler', () => {
	it('debounces lifecycle refreshes and queues one follow-up during an active load', async () => {
		vi.useFakeTimers();
		const deferred: Array<() => void> = [];
		const refreshFn = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					deferred.push(resolve);
				}),
		);
		const scheduler = createSessionRefreshScheduler(refreshFn, 300);

		scheduler.schedule();
		scheduler.schedule();
		await vi.advanceTimersByTimeAsync(300);
		expect(refreshFn).toHaveBeenCalledTimes(1);

		scheduler.schedule();
		scheduler.schedule();
		deferred.shift()?.();
		await Promise.resolve();
		await vi.advanceTimersByTimeAsync(300);
		expect(refreshFn).toHaveBeenCalledTimes(2);

		deferred.shift()?.();
		scheduler.dispose();
		vi.useRealTimers();
	});

	it('does not start a delayed refresh after disposal', async () => {
		vi.useFakeTimers();
		const refreshFn = vi.fn(async () => {});
		const scheduler = createSessionRefreshScheduler(refreshFn, 300);
		scheduler.schedule();
		scheduler.dispose();
		await vi.advanceTimersByTimeAsync(300);
		expect(refreshFn).not.toHaveBeenCalled();
		vi.useRealTimers();
	});
});
