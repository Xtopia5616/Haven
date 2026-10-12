import { describe, expect, it } from 'vitest';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parseShellAvailability,
} from './diagnostics.ts';

describe('diagnostics command contracts', () => {
	it('accepts the log info response shape and a missing current log file', () => {
		expect(
			parseLogInfo({ enabled: true, level: 'debug', path: 'C:/logs/haven.2026-08-26' }),
		).toEqual({ enabled: true, level: 'debug', path: 'C:/logs/haven.2026-08-26' });
		expect(parseLogInfo({ enabled: false, level: 'info', path: null })).toEqual({
			enabled: false,
			level: 'info',
			path: null,
		});
	});

	it('rejects malformed diagnostics responses', () => {
		expect(() => parseLogInfo({ enabled: 'yes', level: 'info', path: null })).toThrow(
			'invalid get_log_info response',
		);
		expect(() => parseLogInfo({ enabled: true, level: 'verbose', path: null })).toThrow(
			'invalid get_log_info response',
		);
		expect(() => parseLogTail({ path: 'x' })).toThrow('invalid read_log_tail response');
		expect(() => parseShellAvailability({ available: 'yes' })).toThrow(
			'invalid check_shell_available response',
		);
	});

	it('accepts log tail and shell availability responses', () => {
		expect(parseLogTail({ path: 'C:/logs/haven.log', content: 'line' })).toEqual({
			path: 'C:/logs/haven.log',
			content: 'line',
		});
		expect(parseShellAvailability({ available: true })).toEqual({ available: true });
	});

	it('validates the explicit API-key status response', () => {
		const status = parseApiKeyStatus({
			models: { default: true },
			providers: { cloud: true },
			stt: false,
			ocr: false,
			ocr_secret: false,
		});
		expect(status.models.default).toBe(true);
		expect(status.providers.cloud).toBe(true);
		expect(() => parseApiKeyStatus({ providers: {} })).toThrow(
			'invalid get_api_key_status response',
		);
	});
});
