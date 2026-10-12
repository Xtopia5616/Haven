import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	checkShellAvailable,
	readApiKeyStatus,
	readLogInfo,
	requestPerformanceMetricsSnapshot,
	readLogTail,
} from './diagnosticsCommands.ts';
import { invoke } from './tauri.ts';
import { AGENT_METRICS_PHASE_COUNT } from './contracts/generatedCommands.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

function metricsSnapshot() {
	return {
		phases: Array.from({ length: AGENT_METRICS_PHASE_COUNT }, () => ({
			count: 1,
			total_ms: 2,
			p50_ms: 5,
			p95_ms: 30,
		})),
		counters: {
			turn_starts: 2,
			first_tokens: 0,
			stream_chunks: 0,
			chunk_drops: 0,
			checkpoint_pending: 0,
			branch_point_failures: 0,
			event_boundary_check_failures: 0,
			projection_failures: 0,
			inbox_ack_failures: 0,
			tool_run_result_retries: 0,
			tool_run_result_duplicates: 0,
			web_search_drops: 0,
		},
		gauges: { context_queue_items: 0 },
		ui: { frames: 3, chunks: 8, drops: 1 },
	};
}

describe('diagnostics command boundary', () => {
	beforeEach(() => {
		vi.mocked(invoke).mockReset();
	});

	it('uses the existing log response parsers and preserves the requested tail limit', async () => {
		vi.mocked(invoke)
			.mockResolvedValueOnce({
				enabled: true,
				level: 'debug',
				path: 'C:/logs/haven.2026-09-26',
			})
			.mockResolvedValueOnce({ path: 'C:/logs/haven.2026-09-26', content: 'first\nsecond' });

		await expect(readLogInfo()).resolves.toEqual({
			enabled: true,
			level: 'debug',
			path: 'C:/logs/haven.2026-09-26',
		});
		await expect(readLogTail({ maxLines: 300 })).resolves.toEqual({
			path: 'C:/logs/haven.2026-09-26',
			content: 'first\nsecond',
		});
		expect(invoke).toHaveBeenNthCalledWith(1, 'get_log_info');
		expect(invoke).toHaveBeenNthCalledWith(2, 'read_log_tail', { maxLines: 300 });
	});

	it('keeps shell and API-key status validation at the same read boundary', async () => {
		vi.mocked(invoke)
			.mockResolvedValueOnce({ available: true } as never)
			.mockResolvedValueOnce({
				models: { 'model-a': true, 'model-b': false },
				providers: { cloud: true },
				stt: false,
				ocr: true,
				ocr_secret: false,
				credential: 'must not escape the existing projection',
			} as never);

		await expect(checkShellAvailable({ shell: 'pwsh' })).resolves.toEqual({ available: true });
		await expect(readApiKeyStatus()).resolves.toEqual({
			models: { 'model-a': true, 'model-b': false },
			providers: { cloud: true },
			stt: false,
			ocr: true,
			ocr_secret: false,
		});
		expect(invoke).toHaveBeenNthCalledWith(1, 'check_shell_available', { shell: 'pwsh' });
		expect(invoke).toHaveBeenNthCalledWith(2, 'get_api_key_status');
	});

	it('validates metric numbers and passes valid snapshots through unchanged', async () => {
		const snapshot = { ...metricsSnapshot(), future_diagnostic: { sample: 'retained' } };
		const withoutUi = { ...metricsSnapshot(), ui: undefined };
		const ui = { frames: 3, chunks: 8, drops: 1 };
		vi.mocked(invoke)
			.mockResolvedValueOnce(snapshot as never)
			.mockResolvedValueOnce(withoutUi as never);

		await expect(requestPerformanceMetricsSnapshot(ui)).resolves.toBe(snapshot);
		await expect(requestPerformanceMetricsSnapshot()).resolves.toEqual(withoutUi);
		expect(invoke).toHaveBeenNthCalledWith(1, 'get_performance_metrics', { ui });
		expect(invoke).toHaveBeenNthCalledWith(2, 'get_performance_metrics', undefined);
	});

	it.each([
		null,
		{},
		{ ...metricsSnapshot(), phases: [null] },
		{ ...metricsSnapshot(), phases: [] },
		{ ...metricsSnapshot(), phases: metricsSnapshot().phases.slice(1) },
		{
			...metricsSnapshot(),
			phases: [...metricsSnapshot().phases, metricsSnapshot().phases[0]],
		},
		{
			...metricsSnapshot(),
			phases: [{ count: Number.MAX_SAFE_INTEGER + 1, total_ms: 0, p50_ms: 0, p95_ms: 0 }],
		},
		{
			...metricsSnapshot(),
			counters: { ...metricsSnapshot().counters, turn_starts: Number.MAX_SAFE_INTEGER + 1 },
		},
		{ ...metricsSnapshot(), gauges: { context_queue_items: -1 } },
		{ ...metricsSnapshot(), ui: null },
	])('rejects malformed or unsafe performance metric values %j', async (response) => {
		vi.mocked(invoke).mockResolvedValueOnce(response as never);
		await expect(requestPerformanceMetricsSnapshot()).rejects.toThrow(
			'Invalid performance metrics response',
		);
	});

	it('propagates command rejection without wrapping it', async () => {
		const failure = new Error('command failed');
		vi.mocked(invoke).mockRejectedValueOnce(failure);

		await expect(readLogInfo()).rejects.toBe(failure);
	});
});
