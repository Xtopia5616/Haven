import { describe, expect, it } from 'vitest';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parseSettingsPayload,
	parseShellAvailability,
} from './settings.ts';

describe('settings command contracts', () => {
	it('validates credential references in the Settings response', () => {
		const payload = {
			llm: {
				providers: [{ api_key_ref: 'cred-11111111111111111111111111111111' }],
			},
			media: {
				ocr: {
					api_key_ref: null,
					api_secret_ref: 'cred-22222222222222222222222222222222',
				},
			},
			mcp_servers: [
				{
					env_refs: [{ credential_ref: 'cred-33333333333333333333333333333333' }],
				},
			],
			future_settings_field: true,
		};

		expect(parseSettingsPayload(payload)).toBe(payload);
	});

	it('rejects malformed credential references wherever Settings persists them', () => {
		for (const invalid of [
			{ llm: { providers: [{ api_key_ref: 'cred-short' }] } },
			{ media: { ocr: { api_secret_ref: 'cred-short' } } },
			{ mcp_servers: [{ env_refs: [{ credential_ref: 'plain-reference' }] }] },
		]) {
			expect(() => parseSettingsPayload(invalid)).toThrow(
				'Invalid Settings credential reference',
			);
		}
	});

	it('rejects Settings numbers that cannot be safely roundtripped through JavaScript', () => {
		const safe = { context_limits: { max_upload_total_bytes: Number.MAX_SAFE_INTEGER } };
		expect(parseSettingsPayload(safe)).toBe(safe);

		for (const invalid of [
			{ context_limits: { max_upload_total_bytes: Number.MAX_SAFE_INTEGER + 1 } },
			{ llm: { retry_jitter: Number.NaN } },
			{ llm: { retry_jitter: Number.POSITIVE_INFINITY } },
		]) {
			expect(() => parseSettingsPayload(invalid)).toThrow('Invalid Settings numeric value');
		}
	});

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
