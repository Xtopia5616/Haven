import { describe, expect, it } from 'vitest';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parseSettingsPayload,
	parseShellAvailability,
} from './settings.ts';
import { createSettingsFixture } from './settings-fixture.ts';

function withSetting(path: readonly string[], value: unknown) {
	const settings = createSettingsFixture();
	let owner: Record<string, unknown> = settings as unknown as Record<string, unknown>;
	for (const segment of path.slice(0, -1)) {
		const child = owner[segment];
		if (typeof child !== 'object' || child === null || Array.isArray(child)) {
			throw new Error(`Invalid Settings test path: ${path.join('.')}`);
		}
		owner = child as Record<string, unknown>;
	}
	owner[path.at(-1)!] = value;
	return settings;
}

describe('settings command contracts', () => {
	it('validates credential references in the full Settings response', () => {
		const payload = createSettingsFixture();
		payload.llm.providers.push({
			name: 'primary',
			provider: 'openai',
			api_style: null,
			base_url: '',
			api_key_ref: 'cred-11111111111111111111111111111111',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
			proxy_url: null,
			no_proxy: null,
		});
		payload.media.ocr.api_secret_ref = 'cred-22222222222222222222222222222222';
		payload.mcp_servers.push({
			name: 'local',
			transport: 'stdio',
			command: 'server',
			args: [],
			env_refs: [
				{
					name: 'TOKEN',
					credential_ref: 'cred-33333333333333333333333333333333',
					has_value: false,
				},
			],
			cwd: null,
			url: '',
			enabled: false,
		});
		const payloadWithFutureField = { ...payload, future_settings_field: true };

		expect(parseSettingsPayload(payloadWithFutureField)).toBe(payloadWithFutureField);
		expect(parseSettingsPayload(null)).toBeNull();
	});

	it('rejects malformed credential references wherever Settings persists them', () => {
		const provider = {
			name: 'primary',
			provider: 'openai',
			api_style: null,
			base_url: '',
			api_key_ref: 'cred-short',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
			proxy_url: null,
			no_proxy: null,
		};
		const badProvider = createSettingsFixture();
		badProvider.llm.providers.push(provider);
		const badOcr = createSettingsFixture();
		badOcr.media.ocr.api_secret_ref = 'cred-short';
		const badMcp = createSettingsFixture();
		badMcp.mcp_servers.push({
			name: 'local',
			transport: 'stdio',
			command: 'server',
			args: [],
			env_refs: [{ name: 'TOKEN', credential_ref: 'plain-reference', has_value: false }],
			cwd: null,
			url: '',
			enabled: false,
		});

		for (const invalid of [badProvider, badOcr, badMcp]) {
			expect(() => parseSettingsPayload(invalid)).toThrow(
				'Invalid Settings credential reference',
			);
		}
	});

	it('rejects incomplete Settings records and unknown closed-enum values', () => {
		expect(() => parseSettingsPayload({})).toThrow('Invalid get_settings response shape');
		expect(() => parseSettingsPayload({ llm: { providers: [] } })).toThrow(
			'Invalid get_settings response shape',
		);
		expect(() => parseSettingsPayload(withSetting(['default_shell'], 'not-a-shell'))).toThrow(
			'Invalid get_settings response shape',
		);
		expect(() =>
			parseSettingsPayload(withSetting(['media', 'input_strategy'], 'not-a-strategy')),
		).toThrow('Invalid get_settings response shape');
		expect(() =>
			parseSettingsPayload(withSetting(['session', 'max_steps_per_session'], null)),
		).toThrow('Invalid get_settings response shape');
	});

	it('rejects Settings numbers that cannot be safely roundtripped through JavaScript', () => {
		const safe = createSettingsFixture();
		safe.context_limits.max_upload_total_bytes = Number.MAX_SAFE_INTEGER;
		safe.context_limits.default_context_window = 0xffff_ffff;
		safe.context_limits.input_ring_buffer_secs = 120;
		safe.media.audio.channels = 0xffff;
		expect(parseSettingsPayload(safe)).toBe(safe);

		for (const [path, invalid] of [
			[['context_limits', 'max_upload_total_bytes'], Number.MAX_SAFE_INTEGER + 1],
			[['context_limits', 'default_context_window'], 1.5],
			[['session', 'max_steps_per_run'], -1],
			[['context_limits', 'default_context_window'], 0x1_0000_0000],
			[['context_limits', 'input_ring_buffer_secs'], 121],
			[['media', 'audio', 'channels'], 0x1_0000],
			[['media', 'stt', 'min_confidence'], 1.1],
			[['llm', 'retry_jitter'], Number.NaN],
			[['llm', 'retry_jitter'], Number.POSITIVE_INFINITY],
		] as const) {
			expect(() => parseSettingsPayload(withSetting(path, invalid))).toThrow(
				'Invalid Settings numeric value',
			);
		}
	});

	it('rejects malformed numeric types and accepts required nullable Settings values', () => {
		for (const [path, invalid] of [
			[['session', 'max_steps_per_run'], '12'],
			[['llm', 'retry_jitter'], '0.5'],
			[['media', 'audio', 'channels'], null],
			[['media', 'stt', 'timeout_secs'], null],
			[['context_limits', 'reasoning_echo_max_chars'], null],
		] as const) {
			expect(() => parseSettingsPayload(withSetting(path, invalid))).toThrow(
				'Invalid Settings numeric value',
			);
		}

		const nullable = createSettingsFixture();
		nullable.skills_exec.cpu_time_secs = null;
		nullable.skills_exec.max_memory_mb = null;
		expect(parseSettingsPayload(nullable)).toBe(nullable);
	});

	it('rejects MCP intervals that can spin or exceed the reconnect cap', () => {
		for (const [field, value] of [
			['health_interval_secs', 0],
			['reconnect_initial_ms', 0],
			['reconnect_max_ms', 0],
		] as const) {
			expect(() =>
				parseSettingsPayload(withSetting(['mcp_discovery', field], value)),
			).toThrow('Invalid Settings numeric value');
		}
		const invalidOrder = createSettingsFixture();
		invalidOrder.mcp_discovery.reconnect_initial_ms = 2_000;
		invalidOrder.mcp_discovery.reconnect_max_ms = 1_000;
		expect(() => parseSettingsPayload(invalidOrder)).toThrow('Invalid Settings numeric value');
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
