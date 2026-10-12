import { describe, expect, it } from 'vitest';
import {
	AUDIO_CAPTURE_BOUNDS,
	CONCURRENCY_BOUNDS,
	CONTEXT_WINDOW_BOUNDS,
	MEDIA_TIMEOUT_BOUNDS,
	RESOURCE_LIMIT_BOUNDS,
	SESSION_STEP_BUDGET_BOUNDS,
	parseSessionPermissionGrantResponses,
	parseSettingsPayload,
	validateSettingsNumbers,
} from './settings.ts';
import { createSettingsFixture } from './settingsFixture.ts';

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
	it('validates permission capability scopes with the Rust owner grammar', () => {
		const grant = {
			session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
			session_title: null,
			capability: 'system.power-lock',
			target: 'operation',
			effect: 'allow',
		};
		const { session_id, ...rest } = grant;
		const [parsedGrant] = parseSessionPermissionGrantResponses([grant]);
		expect(parsedGrant).toEqual({ ...rest, sessionId: session_id });
		expect(parsedGrant).not.toHaveProperty('session_id');

		for (const capability of [
			'',
			' system.power',
			'system.power ',
			'.system',
			'system..power',
			'system:power',
		]) {
			expect(() => parseSessionPermissionGrantResponses([{ ...grant, capability }])).toThrow(
				'Invalid session permission grant identity',
			);
		}
	});

	it('validates credential references in the full Settings response', () => {
		const payload = createSettingsFixture();
		payload.llm.providers.push({
			name: 'primary',
			provider: 'openai',
			api_style: null,
			base_url: '',
			api_key_ref: 'cred-0000008c3defb1edb984fe2ac71c71c7',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
			proxy_url: null,
			no_proxy: null,
		});
		payload.media.ocr.api_secret_ref = 'cred-000001187bdf63db7309fc558e38e38e';
		payload.mcp_servers.push({
			name: 'local',
			transport: 'stdio',
			command: 'server',
			args: [],
			env_refs: [
				{
					name: 'TOKEN',
					credential_ref: 'cred-000001a4b9cf15c92c8efa8055555555',
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

	it('validates media provider references and search mode semantics', () => {
		for (const provider of ['llm', 'baidu', 'azure', 'tencent', 'none']) {
			const valid = withSetting(['media', 'ocr', 'provider'], provider);
			expect(parseSettingsPayload(valid)).toBe(valid);
		}
		expect(() =>
			parseSettingsPayload(withSetting(['media', 'ocr', 'provider'], 'future')),
		).toThrow('settings.media.ocr.provider');

		const provider = {
			name: 'cloud',
			provider: 'openai',
			api_style: null,
			base_url: '',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
			proxy_url: null,
			no_proxy: null,
		};
		for (const mode of [
			'off',
			'auto',
			'always',
			'required',
			'on',
			'1',
			'true',
			' TRUE ',
			'  ',
		]) {
			const valid = createSettingsFixture();
			valid.llm.providers.push({ ...provider, default_web_search: mode });
			valid.llm.models.push({
				id: 'profile',
				provider_name: 'cloud',
				model: 'model',
				capabilities: [],
				web_search: mode,
			});
			expect(parseSettingsPayload(valid)).toBe(valid);
		}
		const invalidWebSearch = createSettingsFixture();
		invalidWebSearch.llm.providers.push({ ...provider, default_web_search: 'alway' });
		expect(() => parseSettingsPayload(invalidWebSearch)).toThrow(
			'settings.llm.providers[0].default_web_search',
		);

		const mcpStt = createSettingsFixture();
		mcpStt.media.stt.provider_name = 'MCP';
		expect(() => parseSettingsPayload(mcpStt)).toThrow('settings.media.stt.mcp_server');
		mcpStt.mcp_servers.push({
			name: 'transcriber',
			transport: 'stdio',
			command: 'server',
			args: [],
			cwd: null,
			url: '',
			enabled: false,
		});
		mcpStt.media.stt.mcp_server = 'transcriber';
		expect(parseSettingsPayload(mcpStt)).toBe(mcpStt);
		mcpStt.media.stt.mcp_server = 'missing';
		expect(() => parseSettingsPayload(mcpStt)).toThrow('settings.media.stt.mcp_server');
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
		safe.context_limits.default_context_window = CONTEXT_WINDOW_BOUNDS.maxTokens;
		safe.context_limits.input_ring_buffer_secs = 120;
		expect(parseSettingsPayload(safe)).toBe(safe);

		for (const [path, invalid] of [
			[['context_limits', 'max_upload_total_bytes'], Number.MAX_SAFE_INTEGER + 1],
			[['context_limits', 'default_context_window'], 1.5],
			[['session', 'max_steps_per_run'], -1],
			[['context_limits', 'default_context_window'], 0x1_0000_0000],
			[['context_limits', 'input_ring_buffer_secs'], 121],
			[['media', 'stt', 'min_confidence'], 1.1],
			[['llm', 'retry_jitter'], Number.NaN],
			[['llm', 'retry_jitter'], Number.POSITIVE_INFINITY],
		] as const) {
			expect(() => parseSettingsPayload(withSetting(path, invalid))).toThrow(
				'Invalid Settings numeric value',
			);
		}
	});

	it('enforces context window bounds and compaction reserve relationships', () => {
		const safe = createSettingsFixture();
		safe.context_limits.default_context_window = CONTEXT_WINDOW_BOUNDS.maxTokens;
		safe.context_limits.response_token_floor = CONTEXT_WINDOW_BOUNDS.maxTokens;
		safe.context_limits.compaction_reserve_tokens = CONTEXT_WINDOW_BOUNDS.maxTokens - 1;
		safe.llm.models.push({
			id: 'small',
			provider_name: 'provider',
			model: 'small',
			capabilities: [],
			context_window: CONTEXT_WINDOW_BOUNDS.maxTokens,
		});
		expect(parseSettingsPayload(safe)).toBe(safe);

		const minWindow = withSetting(
			['context_limits', 'default_context_window'],
			CONTEXT_WINDOW_BOUNDS.minTokens,
		);
		minWindow.context_limits.compaction_reserve_tokens = CONTEXT_WINDOW_BOUNDS.minTokens - 1;
		expect(parseSettingsPayload(minWindow)).toBe(minWindow);
		for (const [path, invalid] of [
			[['context_limits', 'default_context_window'], CONTEXT_WINDOW_BOUNDS.minTokens - 1],
			[['context_limits', 'default_context_window'], CONTEXT_WINDOW_BOUNDS.maxTokens + 1],
			[['context_limits', 'response_token_floor'], CONTEXT_WINDOW_BOUNDS.maxTokens + 1],
		] as const) {
			expect(() => parseSettingsPayload(withSetting(path, invalid))).toThrow(
				`settings.${path.join('.')}`,
			);
		}

		const invalidModel = createSettingsFixture();
		invalidModel.llm.models.push({
			id: 'too-small',
			provider_name: 'provider',
			model: 'small',
			capabilities: [],
			context_window: CONTEXT_WINDOW_BOUNDS.minTokens - 1,
		});
		expect(() => parseSettingsPayload(invalidModel)).toThrow(
			'settings.llm.models[0].context_window',
		);

		const reserveAtDefault = createSettingsFixture();
		reserveAtDefault.context_limits.compaction_reserve_tokens =
			reserveAtDefault.context_limits.default_context_window;
		expect(() => parseSettingsPayload(reserveAtDefault)).toThrow(
			'settings.context_limits.compaction_reserve_tokens',
		);

		const reserveAtModelWindow = createSettingsFixture();
		reserveAtModelWindow.context_limits.compaction_reserve_tokens = 4_000;
		reserveAtModelWindow.llm.models.push({
			id: 'small',
			provider_name: 'provider',
			model: 'small',
			capabilities: [],
			context_window: 4_000,
		});
		expect(() => parseSettingsPayload(reserveAtModelWindow)).toThrow(
			'settings.context_limits.compaction_reserve_tokens',
		);
	});

	it('enforces media timeout ranges in both Settings response and update contracts', () => {
		for (const [kind, bounds] of Object.entries(MEDIA_TIMEOUT_BOUNDS)) {
			for (const value of [bounds.min, bounds.max]) {
				const valid = withSetting(['media', kind, 'timeout_secs'], value);
				expect(parseSettingsPayload(valid)).toBe(valid);
				expect(() => validateSettingsNumbers(valid)).not.toThrow();
			}

			for (const value of new Set([0, bounds.min - 1, bounds.max + 1])) {
				const invalid = withSetting(['media', kind, 'timeout_secs'], value);
				expect(() => parseSettingsPayload(invalid)).toThrow(
					`Invalid Settings numeric value at settings.media.${kind}.timeout_secs`,
				);
				expect(() => validateSettingsNumbers(invalid)).toThrow(
					`Invalid Settings numeric value at settings.media.${kind}.timeout_secs`,
				);
			}
		}
	});

	it('enforces session step budgets and the zero means unlimited UI mapping', () => {
		for (const value of [
			SESSION_STEP_BUDGET_BOUNDS.perRun.min,
			SESSION_STEP_BUDGET_BOUNDS.perRun.max,
		]) {
			const valid = withSetting(['session', 'max_steps_per_run'], value);
			expect(parseSettingsPayload(valid)).toBe(valid);
			expect(() => validateSettingsNumbers(valid)).not.toThrow();
		}

		for (const value of [1, SESSION_STEP_BUDGET_BOUNDS.perSession.max]) {
			const valid = withSetting(['session', 'max_steps_per_session'], value);
			expect(parseSettingsPayload(valid)).toBe(valid);
			expect(() => validateSettingsNumbers(valid)).not.toThrow();
		}

		const unlimited = withSetting(['session', 'max_steps_per_session'], null);
		expect(() => validateSettingsNumbers(unlimited)).not.toThrow();

		for (const [field, values] of [
			['max_steps_per_run', [0, SESSION_STEP_BUDGET_BOUNDS.perRun.max + 1]],
			['max_steps_per_session', [0, SESSION_STEP_BUDGET_BOUNDS.perSession.max + 1]],
		] as const) {
			for (const value of values) {
				const invalid = withSetting(['session', field], value);
				const path = `settings.session.${field}`;
				expect(() => parseSettingsPayload(invalid)).toThrow(path);
				expect(() => validateSettingsNumbers(invalid)).toThrow(path);
			}
		}
	});

	it('enforces audio capture bounds and keeps the Settings audio shape minimal', () => {
		for (const [field, bounds] of Object.entries(AUDIO_CAPTURE_BOUNDS)) {
			const wireField =
				field === 'maxDurationSecs' ? 'max_duration_secs' : 'silence_timeout_ms';
			for (const value of [bounds.min, bounds.max]) {
				const valid = withSetting(['media', 'audio', wireField], value);
				expect(parseSettingsPayload(valid)).toBe(valid);
				expect(() => validateSettingsNumbers(valid)).not.toThrow();
			}
			for (const value of [bounds.min - 1, bounds.max + 1]) {
				const invalid = withSetting(['media', 'audio', wireField], value);
				expect(() => parseSettingsPayload(invalid)).toThrow(
					`settings.media.audio.${wireField}`,
				);
				expect(() => validateSettingsNumbers(invalid)).toThrow(
					`settings.media.audio.${wireField}`,
				);
			}
		}

		expect(createSettingsFixture().media.audio).toEqual({
			max_duration_secs: 60,
			silence_timeout_ms: 1_500,
			vad_threshold: 0.5,
		});
	});

	it('enforces shared resource limits for history, buffers, skills, and file listings', () => {
		const fields = [
			['session', 'prompt_history_limit', RESOURCE_LIMIT_BOUNDS.promptHistory],
			['session', 'history_retention_days', RESOURCE_LIMIT_BOUNDS.historyRetentionDays],
			['context_limits', 'file_max_list_entries', RESOURCE_LIMIT_BOUNDS.fileListEntries],
			['context_limits', 'file_line_max_bytes', RESOURCE_LIMIT_BOUNDS.fileLineBytes],
			['context_limits', 'file_byte_read_max_bytes', RESOURCE_LIMIT_BOUNDS.fileByteReadBytes],
			['context_limits', 'max_known_facts', RESOURCE_LIMIT_BOUNDS.maxKnownFacts],
			['context_limits', 'input_ring_buffer_secs', RESOURCE_LIMIT_BOUNDS.inputRingBufferSecs],
			['context_limits', 'mcp_max_sse_buffer_bytes', RESOURCE_LIMIT_BOUNDS.mcpSseBufferBytes],
			['context_limits', 'skills_max_md_bytes', RESOURCE_LIMIT_BOUNDS.skillMarkdownBytes],
			['context_limits', 'network_max_body_bytes', RESOURCE_LIMIT_BOUNDS.networkBodyBytes],
			['context_limits', 'search_max_results', RESOURCE_LIMIT_BOUNDS.searchResults],
			['context_limits', 'search_window_bytes', RESOURCE_LIMIT_BOUNDS.searchWindowBytes],
			['context_limits', 'embedding_chunk_size', RESOURCE_LIMIT_BOUNDS.embeddingChunk],
			['context_limits', 'max_tools_per_request', RESOURCE_LIMIT_BOUNDS.toolsPerRequest],
		] as const;

		for (const [owner, field, bounds] of fields) {
			for (const value of [bounds.min, bounds.max]) {
				const valid = withSetting([owner, field], value);
				expect(parseSettingsPayload(valid)).toBe(valid);
				expect(() => validateSettingsNumbers(valid)).not.toThrow();
			}
			for (const value of [bounds.min - 1, bounds.max + 1]) {
				const invalid = withSetting([owner, field], value);
				const path = `settings.${owner}.${field}`;
				expect(() => parseSettingsPayload(invalid)).toThrow(path);
				expect(() => validateSettingsNumbers(invalid)).toThrow(path);
			}
		}
	});

	it('enforces concurrency budgets across session, model, and background limits', () => {
		const fields = [
			['session', 'max_concurrent', CONCURRENCY_BOUNDS.sessions],
			['llm', 'max_concurrent_requests', CONCURRENCY_BOUNDS.llmRequests],
			[
				'context_limits',
				'max_running_background_tool_runs',
				CONCURRENCY_BOUNDS.backgroundToolRuns,
			],
		] as const;
		for (const [owner, field, bounds] of fields) {
			for (const value of [bounds.min, bounds.max]) {
				const valid = withSetting([owner, field], value);
				expect(parseSettingsPayload(valid)).toBe(valid);
				expect(() => validateSettingsNumbers(valid)).not.toThrow();
			}
			for (const value of [bounds.min - 1, bounds.max + 1]) {
				const invalid = withSetting([owner, field], value);
				expect(() => parseSettingsPayload(invalid)).toThrow(`${owner}.${field}`);
				expect(() => validateSettingsNumbers(invalid)).toThrow(`${owner}.${field}`);
			}
		}
	});

	it('rejects malformed numeric types and accepts required nullable Settings values', () => {
		for (const [path, invalid] of [
			[['session', 'max_steps_per_run'], '12'],
			[['llm', 'retry_jitter'], '0.5'],
			[['media', 'stt', 'timeout_secs'], null],
			[['context_limits', 'reasoning_echo_max_chars'], null],
		] as const) {
			expect(() => parseSettingsPayload(withSetting(path, invalid))).toThrow(
				'Invalid Settings numeric value',
			);
		}

		expect(parseSettingsPayload(createSettingsFixture())).not.toBeNull();
	});

	it('omits ineffective skill resource limits from the generated Settings contract', () => {
		const skillsExec = createSettingsFixture().skills_exec as unknown as Record<
			string,
			unknown
		>;
		expect(skillsExec).not.toHaveProperty('cpu_time_secs');
		expect(skillsExec).not.toHaveProperty('max_memory_mb');
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
});
