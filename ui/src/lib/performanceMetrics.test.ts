import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	readPerformanceMetricsSnapshot,
	registerPerformanceMetricsProvider,
} from './performanceMetrics.ts';
import type { UiMetricsSnapshot } from './contracts/commands.ts';
import { invoke } from './tauri.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

describe('performance metrics export boundary', () => {
	beforeEach(() => {
		vi.mocked(invoke).mockReset();
	});

	it('exports backend and renderer metrics through one invoke with the live UI snapshot', async () => {
		const ui: UiMetricsSnapshot = { frames: 3, chunks: 8, drops: 1 };
		vi.mocked(invoke).mockResolvedValue({ counters: {}, ui } as never);
		const unregister = registerPerformanceMetricsProvider(() => ui);

		await expect(readPerformanceMetricsSnapshot()).resolves.toEqual({ counters: {}, ui });
		expect(invoke).toHaveBeenCalledWith('get_performance_metrics', { ui });

		unregister();
	});
});
