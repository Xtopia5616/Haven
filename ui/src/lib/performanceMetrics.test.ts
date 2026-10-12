import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	readPerformanceMetricsSnapshot,
	registerPerformanceMetricsProvider,
} from './performanceMetrics.ts';
import type { UiMetricsSnapshot } from './contracts/commands.ts';
import { AGENT_METRICS_PHASE_COUNT } from './contracts/generatedCommands.ts';
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
		vi.mocked(invoke).mockResolvedValue({
			phases: Array.from({ length: AGENT_METRICS_PHASE_COUNT }, () => ({
				count: 0,
				total_ms: 0,
				p50_ms: 0,
				p95_ms: 0,
			})),
			counters: {
				turn_starts: 0,
				first_tokens: 0,
				stream_chunks: 0,
				chunk_drops: 0,
				checkpoint_pending: 0,
				branch_point_failures: 0,
				snapshot_failures: 0,
				projection_failures: 0,
				inbox_ack_failures: 0,
				tool_run_result_retries: 0,
				tool_run_result_duplicates: 0,
				web_search_drops: 0,
			},
			gauges: { context_queue_items: 0 },
			ui,
		} as never);
		const unregister = registerPerformanceMetricsProvider(() => ui);

		await expect(readPerformanceMetricsSnapshot()).resolves.toMatchObject({ ui });
		expect(invoke).toHaveBeenCalledWith('get_performance_metrics', { ui });

		unregister();
	});
});
