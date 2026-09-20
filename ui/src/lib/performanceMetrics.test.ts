import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	getPerformanceMetrics,
	registerPerformanceMetricsProvider,
} from './performanceMetrics.ts';
import type { StreamMetricsSnapshot } from './streamAggregator.ts';
import { invoke } from './tauri.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

describe('performance metrics export boundary', () => {
	beforeEach(() => {
		vi.mocked(invoke).mockReset();
	});

	it('exports backend and renderer metrics through one invoke with the live UI snapshot', async () => {
		const ui: StreamMetricsSnapshot = { frames: 3, chunks: 8, drops: 1 };
		vi.mocked(invoke).mockResolvedValue({ counters: {}, ui });
		const unregister = registerPerformanceMetricsProvider(() => ui);

		await expect(getPerformanceMetrics()).resolves.toEqual({ counters: {}, ui });
		expect(invoke).toHaveBeenCalledWith('get_performance_metrics', { ui });

		unregister();
	});
});
