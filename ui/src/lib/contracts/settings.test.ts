import { describe, expect, it } from 'vitest';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parseShellAvailability,
} from './settings.ts';

describe('settings command contracts', () => {
	it('accepts the log info response shape', () => {
		expect(
			parseLogInfo({ enabled: true, level: 'debug', path: 'C:/logs/haven.2026-08-26' }),
		).toEqual({ enabled: true, level: 'debug', path: 'C:/logs/haven.2026-08-26' });
	});

	it('accepts a missing current log file represented by null', () => {
		expect(parseLogInfo({ enabled: false, level: 'info', path: null })).toEqual({
			enabled: false,
			level: 'info',
			path: null,
		});
	});

	it('rejects malformed diagnostic responses', () => {
		expect(() => parseLogInfo({ enabled: 'yes', level: 'info', path: null })).toThrow(
			'invalid get_log_info response',
		);
		expect(() => parseLogTail({ path: 'x' })).toThrow('invalid read_log_tail response');
		expect(() => parseShellAvailability({ available: 'yes' })).toThrow(
			'invalid check_shell_available response',
		);
	});

	it('accepts the log tail and shell availability responses', () => {
		expect(parseLogTail({ path: 'C:/logs/haven.log', content: 'line' })).toEqual({
			path: 'C:/logs/haven.log',
			content: 'line',
		});
		expect(parseShellAvailability({ available: true })).toEqual({ available: true });
	});

	it('validates the explicit API-key status response', () => {
		const status = parseApiKeyStatus({
			small_model: false,
			default_model: true,
			balanced_model: false,
			image_model: false,
			audio_model: false,
			embedding_model: false,
			providers: { cloud: true },
			stt: false,
			ocr: false,
			ocr_secret: false,
		});
		expect(status.default_model).toBe(true);
		expect(status.providers.cloud).toBe(true);
		expect(() => parseApiKeyStatus({ providers: {} })).toThrow(
			'invalid get_api_key_status response',
		);
	});
});
