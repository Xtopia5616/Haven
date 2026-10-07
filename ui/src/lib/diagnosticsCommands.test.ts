import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	checkShellAvailable,
	readApiKeyStatus,
	readLogInfo,
	requestPerformanceMetricsSnapshot,
	readLogTail,
} from './diagnosticsCommands.ts';
import { invoke } from './tauri.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

describe('diagnostics command boundary', () => {
	beforeEach(() => {
		vi.mocked(invoke).mockReset();
	});

	it('uses the existing log response parsers and preserves the requested tail limit', async () => {
		vi.mocked(invoke)
			.mockResolvedValueOnce({ enabled: true, level: 'debug', path: 'C:/logs/haven.2026-09-26' })
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

	it('passes performance metrics through unchanged, including future diagnostic fields', async () => {
		const snapshot = {
			counters: { turns: 2 },
			ui: { frames: 3, chunks: 8, drops: 1 },
			future_diagnostic: { sample: 'retained' },
		};
		const ui = { frames: 3, chunks: 8, drops: 1 };
		vi.mocked(invoke)
			.mockResolvedValueOnce(snapshot as never)
			.mockResolvedValueOnce(snapshot as never);

		await expect(requestPerformanceMetricsSnapshot(ui)).resolves.toBe(snapshot);
		await expect(requestPerformanceMetricsSnapshot()).resolves.toBe(snapshot);
		expect(invoke).toHaveBeenNthCalledWith(1, 'get_performance_metrics', { ui });
		expect(invoke).toHaveBeenNthCalledWith(2, 'get_performance_metrics', undefined);
	});

	it('propagates command rejection without wrapping it', async () => {
		const failure = new Error('command failed');
		vi.mocked(invoke).mockRejectedValueOnce(failure);

		await expect(readLogInfo()).rejects.toBe(failure);
	});
});
