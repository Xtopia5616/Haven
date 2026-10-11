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
		const safe = {
			context_limits: {
				max_upload_total_bytes: Number.MAX_SAFE_INTEGER,
				default_context_window: 0xffff_ffff,
				input_ring_buffer_secs: 120,
			},
			media: { audio: { channels: 0xffff } },
		};
		expect(parseSettingsPayload(safe)).toBe(safe);

		for (const invalid of [
			{ context_limits: { max_upload_total_bytes: Number.MAX_SAFE_INTEGER + 1 } },
			{ context_limits: { default_context_window: 1.5 } },
			{ session: { max_steps_per_run: -1 } },
			{ context_limits: { default_context_window: 0x1_0000_0000 } },
			{ context_limits: { input_ring_buffer_secs: 121 } },
			{ media: { audio: { channels: 0x1_0000 } } },
			{ media: { stt: { min_confidence: 1.1 } } },
			{ llm: { retry_jitter: Number.NaN } },
			{ llm: { retry_jitter: Number.POSITIVE_INFINITY } },
		]) {
			expect(() => parseSettingsPayload(invalid)).toThrow('Invalid Settings numeric value');
		}
	});

	it('rejects malformed Settings number types before IPC while preserving nullable fields', () => {
		for (const invalid of [
			{ session: { max_steps_per_run: '12' } },
			{ llm: { retry_jitter: '0.5' } },
			{ media: { audio: { channels: null } } },
			{ media: { stt: { timeout_secs: null } } },
			{ context_limits: { reasoning_echo_max_chars: null } },
		]) {
			expect(() => parseSettingsPayload(invalid)).toThrow('Invalid Settings numeric value');
		}

		const nullable = {
			llm: { models: [{ temperature: null, reasoning_echo_max_chars: null }] },
			session: { max_steps_per_session: null },
			skills_exec: { cpu_time_secs: null, max_memory_mb: null },
			tool_settings: { shell: { timeout_secs: null, max_output_chars: null } },
		};
		expect(parseSettingsPayload(nullable)).toBe(nullable);
	});

	it('rejects MCP intervals that can spin or exceed the reconnect cap', () => {
		for (const mcp_discovery of [
			{ health_interval_secs: 0 },
			{ reconnect_initial_ms: 0 },
			{ reconnect_initial_ms: 2_000, reconnect_max_ms: 1_000 },
		]) {
			expect(() => parseSettingsPayload({ mcp_discovery })).toThrow(
				'Invalid Settings numeric value',
			);
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
