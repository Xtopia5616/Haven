import { afterEach, describe, expect, it, vi } from 'vitest';
import { createSessionEventReconciler } from './sessionEventReconciler.ts';

afterEach(() => vi.useRealTimers());

describe('session event reconciler', () => {
	it('checks only the active session while the page is visible', async () => {
		vi.useFakeTimers();
		let sessionId: string | null = null;
		let visible = false;
		const reconcile = vi.fn(async () => {});
		const reconciler = createSessionEventReconciler({
			getActiveSessionId: () => sessionId,
			isVisible: () => visible,
			reconcile,
			intervalMs: 1_000,
		});
		reconciler.start();

		await vi.advanceTimersByTimeAsync(1_000);
		expect(reconcile).not.toHaveBeenCalled();
		sessionId = 'ses-00000000000000000000000000000001';
		visible = true;
		await vi.advanceTimersByTimeAsync(1_000);
		expect(reconcile).toHaveBeenCalledExactlyOnceWith(sessionId);

		visible = false;
		await vi.advanceTimersByTimeAsync(1_000);
		expect(reconcile).toHaveBeenCalledTimes(1);
		reconciler.dispose();
	});

	it('does not overlap cursor checks and stops on dispose', async () => {
		vi.useFakeTimers();
		let finish!: () => void;
		const reconcile = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					finish = resolve;
				}),
		);
		const reconciler = createSessionEventReconciler({
			getActiveSessionId: () => 'ses-00000000000000000000000000000002',
			isVisible: () => true,
			reconcile,
			intervalMs: 1_000,
		});
		reconciler.start();
		await vi.advanceTimersByTimeAsync(2_000);
		expect(reconcile).toHaveBeenCalledTimes(1);

		finish();
		await Promise.resolve();
		await vi.advanceTimersByTimeAsync(1_000);
		expect(reconcile).toHaveBeenCalledTimes(2);
		reconciler.dispose();
		await vi.advanceTimersByTimeAsync(2_000);
		expect(reconcile).toHaveBeenCalledTimes(2);
	});
});
