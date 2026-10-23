/** Settings request and response contracts owned by the Settings UI boundary. */

import type {
	FileWriteModeInput as GeneratedFileWriteModeInput,
	NetworkAccessBoundaryInput as GeneratedNetworkAccessBoundaryInput,
	PermissionResourceScope,
	SecurityConfigInput,
	SettingsSnapshot as GeneratedSettingsSnapshot,
	SessionPermissionGrant as GeneratedSessionPermissionGrant,
	StoredPermission,
	TauriCommandRequest,
} from './generatedCommands.ts';
import {
	SESSION_PERMISSION_GRANT_RESPONSE_SHAPE,
	SETTINGS_RESPONSE_SHAPE,
} from './generatedCommands.ts';
import { findGeneratedWireShapeMismatch } from './generatedWireShape.ts';
import { isRecord } from './objectGuards.ts';
import {
	isBoolean,
	isCapabilityScope,
	isFiniteNumber,
	isOneOf,
	isSafeUnsignedInteger,
	isString,
	isU32,
} from './valueGuards.ts';
import { hasOwnWireField, isCanonicalEntityId } from './wireGuards.ts';

export type SecuritySettingsDraft = Omit<
	SecurityConfigInput,
	'resource_policy' | 'permissions'
> & {
	resource_policy: {
		files: {
			write_mode: GeneratedFileWriteModeInput;
			write_roots: string[];
			path_roots: string[];
		};
		network: {
			access: GeneratedNetworkAccessBoundaryInput;
			allowed_domains: string[];
		};
	};
	permissions: Array<StoredPermission & { resource_scope?: PermissionResourceScope[] }>;
};

const U32_FIELDS = new Set([
	'context_window',
	'default_max_tokens',
	'default_context_window',
	'fact_infer_interval_steps',
	'history_retention_days',
	'incomplete_tool_args_retries',
	'max_attachment_image_dim_px',
	'max_retries',
	'max_steps_per_run',
	'max_steps_per_session',
	'max_tokens',
	'network_max_retries',
	'reconnect_max_retries',
	'response_token_floor',
	'retry_factor',
	'retry_max_retries',
	'compaction_reserve_tokens',
]);

const FLOAT_RANGES = new Map<string, readonly [number, number]>([
	['attachment_image_jpeg_quality', [0, 1]],
	['compaction_ratio', [0.1, 0.95]],
	['min_confidence', [0, 1]],
	['retry_jitter', [0, 1]],
	['vad_threshold', [0, 1]],
]);

const FLOAT_NONNEGATIVE_FIELDS = new Set([
	'cost_per_1k_cache_read_tokens',
	'cost_per_1k_cache_write_tokens',
	'cost_per_1k_input_tokens',
	'cost_per_1k_output_tokens',
]);

// Keep this list aligned with the numeric properties in generated SettingsSnapshot
// and SettingsPatchInput. Checking by key catches strings/objects that Rust's typed
// IPC decoder would reject only after the renderer has invoked the command.
const NUMERIC_FIELDS = new Set([
	'attachment_image_jpeg_quality',
	'foreground_tool_output_emit_interval_ms',
	'foreground_tool_output_tail_max_chars',
	'clipboard_entry_max_chars',
	'clipboard_history_entries',
	'clipboard_history_max_entries',
	'compaction_ratio',
	'compaction_reserve_tokens',
	'context_window',
	'cost_per_1k_cache_read_tokens',
	'cost_per_1k_cache_write_tokens',
	'cost_per_1k_input_tokens',
	'cost_per_1k_output_tokens',
	'default_context_window',
	'default_max_tokens',
	'default_timeout_secs',
	'default_timeout_streaming_secs',
	'embedding_chunk_size',
	'event_chunk_batch_max_bytes',
	'fact_extraction_min_interval_secs',
	'fact_infer_interval_steps',
	'file_byte_read_max_bytes',
	'file_line_max_bytes',
	'file_line_span',
	'file_max_full_file_bytes',
	'file_max_patch_bytes',
	'file_max_list_entries',
	'file_summary_input_chars',
	'file_summary_timeout_secs',
	'file_vision_max_bytes',
	'file_write_max_bytes',
	'history_retention_days',
	'health_interval_secs',
	'incomplete_tool_args_retries',
	'input_ring_buffer_secs',
	'max_attachment_file_bytes',
	'max_attachment_files',
	'max_attachment_image_bytes',
	'max_attachment_image_dim_px',
	'max_attachment_images',
	'max_concurrent',
	'max_concurrent_requests',
	'max_duration_secs',
	'max_known_facts',
	'max_observation_chars',
	'max_output_chars',
	'max_output_lines',
	'max_retries',
	'max_running_background_tool_runs',
	'max_steps_per_run',
	'max_steps_per_session',
	'max_tokens',
	'max_tools_per_request',
	'max_total_duration_secs',
	'max_transcript_chars',
	'max_upload_total_bytes',
	'mcp_max_binary_base64_bytes',
	'mcp_max_sse_buffer_bytes',
	'min_confidence',
	'network_backoff_base_secs',
	'network_max_body_bytes',
	'network_max_retries',
	'notification_summary_chars',
	'partial_checkpoint_interval_secs',
	'partial_checkpoint_min_chars',
	'prompt_history_limit',
	'reasoning_echo_max_chars',
	'reconnect_initial_ms',
	'reconnect_max_ms',
	'reconnect_max_retries',
	'response_token_floor',
	'retry_backoff_secs',
	'retry_base_secs',
	'retry_factor',
	'retry_jitter',
	'retry_max_retries',
	'retry_max_secs',
	'sanitize_field_max_chars',
	'search_max_file_size_bytes',
	'search_max_results',
	'search_snippet_chars',
	'search_window_bytes',
	'self_tool_max_instructions_bytes',
	'self_tool_max_script_bytes',
	'silence_timeout_ms',
	'skills_max_line_bytes',
	'skills_max_md_bytes',
	'skills_max_parse_lines',
	'stream_idle_timeout_secs',
	'stream_stall_warn_delay_ms',
	'timeout_secs',
	'tool_run_result_context_chars',
	'tool_run_terminal_ttl_secs',
	'turn_deadline_secs',
	'vad_threshold',
]);

const NULLABLE_NUMERIC_FIELDS = new Set([
	'context_window',
	'cost_per_1k_cache_read_tokens',
	'cost_per_1k_cache_write_tokens',
	'cost_per_1k_input_tokens',
	'cost_per_1k_output_tokens',
	'default_max_tokens',
	'default_timeout_secs',
	'default_timeout_streaming_secs',
	'max_steps_per_session',
	'max_tokens',
	'max_output_chars',
	'max_retries',
	'retry_backoff_secs',
]);

/** Keep these ranges aligned with the Rust Settings/config contract. */
export const MEDIA_TIMEOUT_BOUNDS = {
	stt: { min: 5, max: 600 },
	ocr: { min: 5, max: 300 },
	tts: { min: 5, max: 300 },
	image_gen: { min: 10, max: 600 },
} as const;

/** Keep aligned with the shared Rust SessionConfig validator. */
export const SESSION_STEP_BUDGET_BOUNDS = {
	perRun: { min: 1, max: 1_000 },
	perSession: { minInput: 0, minValue: 1, max: 100_000 },
} as const;

/** Keep aligned with Common AudioConfig validation and Settings controls. */
export const AUDIO_CAPTURE_BOUNDS = {
	maxDurationSecs: { min: 10, max: 300 },
	silenceTimeoutMs: { min: 500, max: 10_000 },
} as const;

/** Keep aligned with Common context-window config validation and model controls. */
export const CONTEXT_WINDOW_BOUNDS = {
	minTokens: 1_000,
	maxTokens: 2_000_000,
} as const;

/** Keep aligned with the Common concurrency contract and Settings controls. */
export const CONCURRENCY_BOUNDS = {
	sessions: { min: 1, max: 10 },
	llmRequests: { min: 1, max: 16 },
	backgroundToolRuns: { min: 0, max: 64 },
} as const;

/** Keep aligned with Common config validation and resource-limit controls. */
export const RESOURCE_LIMIT_BOUNDS = {
	promptHistory: { min: 0, max: 500 },
	historyRetentionDays: { min: 0, max: 365 },
	contextTextChars: { min: 0, max: 100_000 },
	notificationSummaryChars: { min: 0, max: 4_096 },
	sanitizeFieldChars: { min: 1, max: 256 },
	fileListEntries: { min: 0, max: 10_000 },
	fileLineBytes: { min: 1, max: 128_000 },
	fileByteReadBytes: { min: 1, max: 64 * 1024 * 1024 },
	maxKnownFacts: { min: 0, max: 64 },
	inputRingBufferSecs: { min: 0, max: 120 },
	fileOperationBytes: { min: 0, max: 64 * 1024 * 1024 },
	filePatchBytes: { min: 1, max: 64 * 1024 * 1024 },
	fileWriteBytes: { min: 1, max: 64 * 1024 * 1024 },
	fileSummaryInputChars: { min: 0, max: 256_000 },
	fileSummaryTimeoutSecs: { min: 1, max: 600 },
	searchSnippetChars: { min: 0, max: 4_096 },
	searchFileSizeBytes: { min: 0, max: 1024 * 1024 * 1024 },
	mcpSseBufferBytes: { min: 0, max: 4 * 1024 * 1024 },
	mcpBinaryBytes: { min: 0, max: 4 * 1024 * 1024 },
	skillMarkdownBytes: { min: 0, max: 1024 * 1024 },
	selfToolInstructionsBytes: { min: 0, max: 512 * 1024 },
	selfToolScriptBytes: { min: 0, max: 1024 * 1024 },
	networkBodyBytes: { min: 0, max: 16 * 1024 * 1024 },
	networkRetries: { min: 0, max: 10 },
	networkBackoffSecs: { min: 0, max: 60 },
	searchResults: { min: 1, max: 1_000 },
	searchWindowBytes: { min: 0, max: 16 * 1024 * 1024 },
	embeddingChunk: { min: 1, max: 10 },
	toolsPerRequest: { min: 1, max: 350 },
	clipboardEntries: { min: 0, max: 100 },
	clipboardEntryChars: { min: 0, max: 100_000 },
	eventChunkBatchBytes: { min: 0, max: 256 * 1024 },
	mcpHealthIntervalSecs: { min: 1, max: 3_600 },
	mcpReconnectDelayMs: { min: 1, max: 600_000 },
	mcpReconnectRetries: { min: 0, max: 100 },
	skillExecTimeoutSecs: { min: 1, max: 3_600 },
	skillExecOutputLines: { min: 0, max: 50_000 },
	toolTimeoutSecs: { min: 1, max: 3_600 },
	toolRetries: { min: 0, max: 10 },
	toolRetryBackoffSecs: { min: 0, max: 60 },
} as const;

export type SessionPermissionGrant = Omit<GeneratedSessionPermissionGrant, 'session_id'> & {
	sessionId: string;
};

/** Exact Rust-owned config shape from the generated IPC contract. */
export type SettingsPayload = GeneratedSettingsSnapshot;

/** Rust-owned settings input shape, including Serde defaults for omitted fields. */
export type SettingsUpdatePayload = TauriCommandRequest<'update_settings'>['settings'];

function hasValidCredentialReference(value: unknown): boolean {
	return value === undefined || value === null || isCanonicalEntityId(value, 'cred');
}

function hasValidCredentialReferenceField(owner: unknown, field: string): boolean {
	return (
		!isRecord(owner) ||
		!hasOwnWireField(owner, field) ||
		hasValidCredentialReference(owner[field])
	);
}

function hasValidSettingsCredentialReferences(value: Record<string, unknown>): boolean {
	const llm = isRecord(value.llm) ? value.llm : null;
	const providers = llm && Array.isArray(llm.providers) ? llm.providers : [];
	if (!providers.every((provider) => hasValidCredentialReferenceField(provider, 'api_key_ref'))) {
		return false;
	}

	const media = isRecord(value.media) ? value.media : null;
	const ocr = media && isRecord(media.ocr) ? media.ocr : null;
	if (
		!hasValidCredentialReferenceField(ocr, 'api_key_ref') ||
		!hasValidCredentialReferenceField(ocr, 'api_secret_ref')
	) {
		return false;
	}

	const servers = Array.isArray(value.mcp_servers) ? value.mcp_servers : [];
	return servers.every((server) => {
		if (!isRecord(server) || !Array.isArray(server.env_refs)) return true;
		return server.env_refs.every((reference) =>
			hasValidCredentialReferenceField(reference, 'credential_ref'),
		);
	});
}

function findInvalidSettingsNumber(
	value: unknown,
	field?: string,
	path = 'settings',
): string | null {
	if (value === undefined) return null;
	if (value === null) {
		return field !== undefined &&
			NUMERIC_FIELDS.has(field) &&
			!isNullableNumericField(field, path)
			? path
			: null;
	}
	if (field !== undefined && NUMERIC_FIELDS.has(field) && typeof value !== 'number') return path;
	if (typeof value === 'number') {
		const floatRange = field === undefined ? undefined : FLOAT_RANGES.get(field);
		if (floatRange) {
			return isFiniteNumber(value) && value >= floatRange[0] && value <= floatRange[1]
				? null
				: path;
		}
		if (field !== undefined && FLOAT_NONNEGATIVE_FIELDS.has(field)) {
			return isFiniteNumber(value) && value >= 0 ? null : path;
		}
		if (field !== undefined && U32_FIELDS.has(field)) return isU32(value) ? null : path;
		if (!isSafeUnsignedInteger(value)) return path;
		if (
			field === 'input_ring_buffer_secs' &&
			(value < RESOURCE_LIMIT_BOUNDS.inputRingBufferSecs.min ||
				value > RESOURCE_LIMIT_BOUNDS.inputRingBufferSecs.max)
		)
			return path;
		return null;
	}
	if (Array.isArray(value)) {
		for (const [index, item] of value.entries()) {
			const invalidPath = findInvalidSettingsNumber(item, field, `${path}[${index}]`);
			if (invalidPath) return invalidPath;
		}
		return null;
	}
	if (isRecord(value)) {
		for (const [key, item] of Object.entries(value)) {
			const invalidPath = findInvalidSettingsNumber(item, key, `${path}.${key}`);
			if (invalidPath) return invalidPath;
		}
	}
	return null;
}

function isNullableNumericField(field: string, path: string): boolean {
	if (NULLABLE_NUMERIC_FIELDS.has(field)) return true;
	if (field === 'reasoning_echo_max_chars') return path.startsWith('settings.llm.models[');
	if (field === 'timeout_secs') return path.startsWith('settings.tool_settings.');
	return false;
}

function findInvalidMcpInterval(value: unknown): string | null {
	if (!isRecord(value) || !isRecord(value.mcp_discovery)) return null;
	const mcp = value.mcp_discovery;
	const health = mcp.health_interval_secs;
	if (
		health !== undefined &&
		(!isSafeUnsignedInteger(health) ||
			health < RESOURCE_LIMIT_BOUNDS.mcpHealthIntervalSecs.min ||
			health > RESOURCE_LIMIT_BOUNDS.mcpHealthIntervalSecs.max)
	) {
		return 'settings.mcp_discovery.health_interval_secs';
	}
	const retries = mcp.reconnect_max_retries;
	if (
		retries !== undefined &&
		(!isSafeUnsignedInteger(retries) || retries > RESOURCE_LIMIT_BOUNDS.mcpReconnectRetries.max)
	) {
		return 'settings.mcp_discovery.reconnect_max_retries';
	}
	const initial = mcp.reconnect_initial_ms;
	if (
		initial !== undefined &&
		(!isSafeUnsignedInteger(initial) ||
			initial < RESOURCE_LIMIT_BOUNDS.mcpReconnectDelayMs.min ||
			initial > RESOURCE_LIMIT_BOUNDS.mcpReconnectDelayMs.max)
	) {
		return 'settings.mcp_discovery.reconnect_initial_ms';
	}
	const max = mcp.reconnect_max_ms;
	if (
		max !== undefined &&
		(!isSafeUnsignedInteger(max) ||
			max < RESOURCE_LIMIT_BOUNDS.mcpReconnectDelayMs.min ||
			max > RESOURCE_LIMIT_BOUNDS.mcpReconnectDelayMs.max)
	) {
		return 'settings.mcp_discovery.reconnect_max_ms';
	}
	return isSafeUnsignedInteger(initial) && isSafeUnsignedInteger(max) && max < initial
		? 'settings.mcp_discovery.reconnect_max_ms'
		: null;
}

function findInvalidMediaTimeout(value: unknown): string | null {
	if (!isRecord(value) || !isRecord(value.media)) return null;
	for (const [kind, bounds] of Object.entries(MEDIA_TIMEOUT_BOUNDS)) {
		const config = value.media[kind];
		if (!isRecord(config) || config.timeout_secs === undefined) continue;
		const timeout = config.timeout_secs;
		if (!isSafeUnsignedInteger(timeout) || timeout < bounds.min || timeout > bounds.max) {
			return `settings.media.${kind}.timeout_secs`;
		}
	}
	return null;
}

function findInvalidSessionStepBudget(value: unknown): string | null {
	if (!isRecord(value) || !isRecord(value.session)) return null;
	const { max_steps_per_run: perRun, max_steps_per_session: perSession } = value.session;
	if (
		perRun !== undefined &&
		(!isSafeUnsignedInteger(perRun) ||
			perRun < SESSION_STEP_BUDGET_BOUNDS.perRun.min ||
			perRun > SESSION_STEP_BUDGET_BOUNDS.perRun.max)
	) {
		return 'settings.session.max_steps_per_run';
	}
	if (
		perSession !== undefined &&
		perSession !== null &&
		(!isSafeUnsignedInteger(perSession) ||
			perSession < SESSION_STEP_BUDGET_BOUNDS.perSession.minValue ||
			perSession > SESSION_STEP_BUDGET_BOUNDS.perSession.max)
	) {
		return 'settings.session.max_steps_per_session';
	}
	return null;
}

function findInvalidAudioCapture(value: unknown): string | null {
	if (!isRecord(value) || !isRecord(value.media) || !isRecord(value.media.audio)) return null;
	const { max_duration_secs: duration, silence_timeout_ms: silence } = value.media.audio;
	if (
		duration !== undefined &&
		(!isSafeUnsignedInteger(duration) ||
			duration < AUDIO_CAPTURE_BOUNDS.maxDurationSecs.min ||
			duration > AUDIO_CAPTURE_BOUNDS.maxDurationSecs.max)
	) {
		return 'settings.media.audio.max_duration_secs';
	}
	if (
		silence !== undefined &&
		(!isSafeUnsignedInteger(silence) ||
			silence < AUDIO_CAPTURE_BOUNDS.silenceTimeoutMs.min ||
			silence > AUDIO_CAPTURE_BOUNDS.silenceTimeoutMs.max)
	) {
		return 'settings.media.audio.silence_timeout_ms';
	}
	return null;
}

function findInvalidConcurrency(value: unknown): string | null {
	if (!isRecord(value)) return null;
	if (isRecord(value.session) && value.session.max_concurrent !== undefined) {
		const sessions = value.session.max_concurrent;
		if (
			!isSafeUnsignedInteger(sessions) ||
			sessions < CONCURRENCY_BOUNDS.sessions.min ||
			sessions > CONCURRENCY_BOUNDS.sessions.max
		) {
			return 'settings.session.max_concurrent';
		}
	}
	if (isRecord(value.llm) && value.llm.max_concurrent_requests !== undefined) {
		const requests = value.llm.max_concurrent_requests;
		if (
			!isSafeUnsignedInteger(requests) ||
			requests < CONCURRENCY_BOUNDS.llmRequests.min ||
			requests > CONCURRENCY_BOUNDS.llmRequests.max
		) {
			return 'settings.llm.max_concurrent_requests';
		}
	}
	if (
		isRecord(value.context_limits) &&
		value.context_limits.max_running_background_tool_runs !== undefined
	) {
		const runs = value.context_limits.max_running_background_tool_runs;
		if (
			!isSafeUnsignedInteger(runs) ||
			runs < CONCURRENCY_BOUNDS.backgroundToolRuns.min ||
			runs > CONCURRENCY_BOUNDS.backgroundToolRuns.max
		) {
			return 'settings.context_limits.max_running_background_tool_runs';
		}
	}
	return null;
}

function findInvalidResourceLimits(value: unknown): string | null {
	if (!isRecord(value)) return null;
	if (isRecord(value.session) && value.session.history_retention_days !== undefined) {
		const days = value.session.history_retention_days;
		if (
			!isSafeUnsignedInteger(days) ||
			days < RESOURCE_LIMIT_BOUNDS.historyRetentionDays.min ||
			days > RESOURCE_LIMIT_BOUNDS.historyRetentionDays.max
		) {
			return 'settings.session.history_retention_days';
		}
	}
	if (isRecord(value.session) && value.session.prompt_history_limit !== undefined) {
		const history = value.session.prompt_history_limit;
		if (
			!isSafeUnsignedInteger(history) ||
			history < RESOURCE_LIMIT_BOUNDS.promptHistory.min ||
			history > RESOURCE_LIMIT_BOUNDS.promptHistory.max
		) {
			return 'settings.session.prompt_history_limit';
		}
	}
	if (isRecord(value.context_limits)) {
		for (const [field, bounds] of [
			['max_observation_chars', RESOURCE_LIMIT_BOUNDS.contextTextChars],
			['max_known_facts', RESOURCE_LIMIT_BOUNDS.maxKnownFacts],
			['max_transcript_chars', RESOURCE_LIMIT_BOUNDS.contextTextChars],
			['tool_run_result_context_chars', RESOURCE_LIMIT_BOUNDS.contextTextChars],
			['notification_summary_chars', RESOURCE_LIMIT_BOUNDS.notificationSummaryChars],
			['sanitize_field_max_chars', RESOURCE_LIMIT_BOUNDS.sanitizeFieldChars],
			['file_max_full_file_bytes', RESOURCE_LIMIT_BOUNDS.fileOperationBytes],
			['file_max_patch_bytes', RESOURCE_LIMIT_BOUNDS.filePatchBytes],
			['file_write_max_bytes', RESOURCE_LIMIT_BOUNDS.fileWriteBytes],
			['file_vision_max_bytes', RESOURCE_LIMIT_BOUNDS.fileOperationBytes],
			['file_summary_input_chars', RESOURCE_LIMIT_BOUNDS.fileSummaryInputChars],
			['file_summary_timeout_secs', RESOURCE_LIMIT_BOUNDS.fileSummaryTimeoutSecs],
			['file_max_list_entries', RESOURCE_LIMIT_BOUNDS.fileListEntries],
			['file_line_max_bytes', RESOURCE_LIMIT_BOUNDS.fileLineBytes],
			['file_byte_read_max_bytes', RESOURCE_LIMIT_BOUNDS.fileByteReadBytes],
			['search_snippet_chars', RESOURCE_LIMIT_BOUNDS.searchSnippetChars],
			['search_max_file_size_bytes', RESOURCE_LIMIT_BOUNDS.searchFileSizeBytes],
			['mcp_max_binary_base64_bytes', RESOURCE_LIMIT_BOUNDS.mcpBinaryBytes],
			['mcp_max_sse_buffer_bytes', RESOURCE_LIMIT_BOUNDS.mcpSseBufferBytes],
			['skills_max_md_bytes', RESOURCE_LIMIT_BOUNDS.skillMarkdownBytes],
			['self_tool_max_instructions_bytes', RESOURCE_LIMIT_BOUNDS.selfToolInstructionsBytes],
			['self_tool_max_script_bytes', RESOURCE_LIMIT_BOUNDS.selfToolScriptBytes],
			['network_max_body_bytes', RESOURCE_LIMIT_BOUNDS.networkBodyBytes],
			['network_max_retries', RESOURCE_LIMIT_BOUNDS.networkRetries],
			['network_backoff_base_secs', RESOURCE_LIMIT_BOUNDS.networkBackoffSecs],
			['search_max_results', RESOURCE_LIMIT_BOUNDS.searchResults],
			['search_window_bytes', RESOURCE_LIMIT_BOUNDS.searchWindowBytes],
			['embedding_chunk_size', RESOURCE_LIMIT_BOUNDS.embeddingChunk],
			['max_tools_per_request', RESOURCE_LIMIT_BOUNDS.toolsPerRequest],
			['clipboard_history_entries', RESOURCE_LIMIT_BOUNDS.clipboardEntries],
			['clipboard_history_max_entries', RESOURCE_LIMIT_BOUNDS.clipboardEntries],
			['clipboard_entry_max_chars', RESOURCE_LIMIT_BOUNDS.clipboardEntryChars],
			['event_chunk_batch_max_bytes', RESOURCE_LIMIT_BOUNDS.eventChunkBatchBytes],
		] as const) {
			const limit = value.context_limits[field];
			if (
				limit !== undefined &&
				(!isSafeUnsignedInteger(limit) || limit < bounds.min || limit > bounds.max)
			) {
				return `settings.context_limits.${field}`;
			}
		}
	}
	if (isRecord(value.skills_exec)) {
		for (const [field, bounds] of [
			['timeout_secs', RESOURCE_LIMIT_BOUNDS.skillExecTimeoutSecs],
			['max_output_lines', RESOURCE_LIMIT_BOUNDS.skillExecOutputLines],
		] as const) {
			const limit = value.skills_exec[field];
			if (
				limit !== undefined &&
				(!isSafeUnsignedInteger(limit) || limit < bounds.min || limit > bounds.max)
			) {
				return `settings.skills_exec.${field}`;
			}
		}
	}
	if (isRecord(value.tool_settings)) {
		for (const [name, tool] of Object.entries(value.tool_settings)) {
			if (!isRecord(tool)) continue;
			for (const [field, bounds] of [
				['timeout_secs', RESOURCE_LIMIT_BOUNDS.toolTimeoutSecs],
				['max_retries', RESOURCE_LIMIT_BOUNDS.toolRetries],
				['retry_backoff_secs', RESOURCE_LIMIT_BOUNDS.toolRetryBackoffSecs],
			] as const) {
				const limit = tool[field];
				if (
					limit !== undefined &&
					(!isSafeUnsignedInteger(limit) || limit < bounds.min || limit > bounds.max)
				) {
					return `settings.tool_settings.${name}.${field}`;
				}
			}
			const outputLimit = tool.max_output_chars;
			const observationLimit = isRecord(value.context_limits)
				? value.context_limits.max_observation_chars
				: undefined;
			if (
				outputLimit !== undefined &&
				isSafeUnsignedInteger(outputLimit) &&
				isSafeUnsignedInteger(observationLimit) &&
				outputLimit > observationLimit
			) {
				return `settings.tool_settings.${name}.max_output_chars`;
			}
		}
	}
	return null;
}

function findInvalidContextWindows(value: unknown): string | null {
	if (!isRecord(value)) return null;
	const limits = isRecord(value.context_limits) ? value.context_limits : null;
	const defaultWindow = limits?.default_context_window;
	if (
		defaultWindow !== undefined &&
		(!isSafeUnsignedInteger(defaultWindow) ||
			defaultWindow < CONTEXT_WINDOW_BOUNDS.minTokens ||
			defaultWindow > CONTEXT_WINDOW_BOUNDS.maxTokens)
	) {
		return 'settings.context_limits.default_context_window';
	}
	const responseFloor = limits?.response_token_floor;
	if (
		responseFloor !== undefined &&
		(!isSafeUnsignedInteger(responseFloor) || responseFloor > CONTEXT_WINDOW_BOUNDS.maxTokens)
	) {
		return 'settings.context_limits.response_token_floor';
	}
	const reserve = limits?.compaction_reserve_tokens;
	if (
		reserve !== undefined &&
		defaultWindow !== undefined &&
		isSafeUnsignedInteger(reserve) &&
		isSafeUnsignedInteger(defaultWindow) &&
		reserve >= defaultWindow
	) {
		return 'settings.context_limits.compaction_reserve_tokens';
	}

	const models = isRecord(value.llm) && Array.isArray(value.llm.models) ? value.llm.models : [];
	for (const [index, model] of models.entries()) {
		if (!isRecord(model) || model.context_window == null) continue;
		const window = model.context_window;
		if (
			!isSafeUnsignedInteger(window) ||
			window < CONTEXT_WINDOW_BOUNDS.minTokens ||
			window > CONTEXT_WINDOW_BOUNDS.maxTokens
		) {
			return `settings.llm.models[${index}].context_window`;
		}
		if (reserve !== undefined && isSafeUnsignedInteger(reserve) && reserve >= window) {
			return 'settings.context_limits.compaction_reserve_tokens';
		}
	}
	return null;
}

function isSupportedWebSearchSetting(value: unknown): boolean {
	if (typeof value !== 'string') return false;
	const mode = value.trim().toLowerCase();
	return (
		mode === '' ||
		mode === 'off' ||
		mode === 'auto' ||
		mode === 'always' ||
		mode === 'required' ||
		mode === 'on' ||
		mode === '1' ||
		mode === 'true'
	);
}

function isValidMmddyyyy(value: string): boolean {
	const match = /^(\d{2})\/(\d{2})\/(\d{4})$/.exec(value);
	if (!match) return false;
	const month = Number(match[1]);
	const day = Number(match[2]);
	const year = Number(match[3]);
	const date = new Date(Date.UTC(year, month - 1, day));
	return (
		month >= 1 &&
		month <= 12 &&
		year >= 1 &&
		date.getUTCFullYear() === year &&
		date.getUTCMonth() === month - 1 &&
		date.getUTCDate() === day
	);
}

function isValidIpAddress(value: string): boolean {
	if (!value || value.length > 45) return false;
	if (!value.includes(':')) {
		const parts = value.split('.');
		return (
			parts.length === 4 &&
			parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255)
		);
	}
	try {
		return new URL(`http://[${value}]/`).hostname.length > 2;
	} catch {
		return false;
	}
}

function isIndependentSearchConfig(value: unknown, provider: unknown): boolean {
	if (!isRecord(value) || typeof value.provider !== 'string') return false;
	const providerId = typeof provider === 'string' ? provider.trim().toLowerCase() : '';
	switch (value.provider) {
		case 'deepseek_search':
			return (
				providerId === 'deepseek' &&
				isSafeUnsignedInteger(value.count) &&
				value.count >= 1 &&
				value.count <= 50 &&
				typeof value.need_content === 'boolean' &&
				typeof value.pass_original_question === 'boolean' &&
				(value.localization == null ||
					(typeof value.localization === 'string' &&
						isValidIpAddress(value.localization)))
			);
		case 'perplexity_search': {
			const optionalString = (candidate: unknown) =>
				candidate == null || typeof candidate === 'string';
			const validDate = (candidate: unknown) =>
				candidate == null || (typeof candidate === 'string' && isValidMmddyyyy(candidate));
			const validTokenLimit = (candidate: unknown) =>
				candidate == null ||
				(isSafeUnsignedInteger(candidate) && candidate >= 1 && candidate <= 1_000_000);
			const languageFilters = value.search_language_filter;
			const domainFilters = value.search_domain_filter;
			return (
				providerId === 'perplexity' &&
				isSafeUnsignedInteger(value.max_results) &&
				value.max_results >= 1 &&
				value.max_results <= (value.search_type === 'people' ? 50 : 20) &&
				['web', 'fast', 'people'].includes(String(value.search_type)) &&
				(value.recency_filter == null ||
					['hour', 'day', 'week', 'month', 'year'].includes(
						String(value.recency_filter),
					)) &&
				optionalString(value.country) &&
				(value.country == null || /^[A-Za-z]{2}$/.test(value.country)) &&
				validTokenLimit(value.max_tokens) &&
				validTokenLimit(value.max_tokens_per_page) &&
				Array.isArray(languageFilters) &&
				languageFilters.length <= 20 &&
				languageFilters.every(
					(language) => typeof language === 'string' && /^[A-Za-z]{2}$/.test(language),
				) &&
				Array.isArray(domainFilters) &&
				domainFilters.length <= 20 &&
				domainFilters.every(
					(domain) =>
						typeof domain === 'string' &&
						domain.length > 0 &&
						domain.length <= 253 &&
						!/\s/.test(domain),
				) &&
				validDate(value.last_updated_after_filter) &&
				validDate(value.last_updated_before_filter) &&
				validDate(value.search_after_date_filter) &&
				validDate(value.search_before_date_filter)
			);
		}
		case 'aliyun_ai_search': {
			let validEndpoint = false;
			if (typeof value.endpoint === 'string') {
				try {
					const parsed = new URL(value.endpoint);
					validEndpoint =
						parsed.protocol === 'https:' &&
						!parsed.username &&
						!parsed.password &&
						!parsed.search &&
						!parsed.hash;
				} catch {
					validEndpoint = false;
				}
			}
			return (
				providerId === 'dashscope' &&
				validEndpoint &&
				typeof value.workspace_name === 'string' &&
				/^[A-Za-z0-9_-]{1,128}$/.test(value.workspace_name) &&
				isSafeUnsignedInteger(value.top_k) &&
				value.top_k >= 1 &&
				value.top_k <= 50 &&
				typeof value.query_rewrite === 'boolean' &&
				['snippet', 'summary', 'mainText'].includes(String(value.content_type)) &&
				['lite', 'pro', 'pro-fetch'].includes(String(value.way)) &&
				(value.content_type === 'mainText' || value.way !== 'pro-fetch') &&
				['day', 'week', 'month', 'year', 'no_limit'].includes(String(value.time_range)) &&
				isCanonicalEntityId(value.api_key_ref, 'cred')
			);
		}
		case 'mistral_web_search':
			return providerId === 'mistral' && typeof value.premium === 'boolean';
		case 'zhipu_web_search':
			return providerId === 'zhipu' && Object.keys(value).every((key) => key === 'provider');
		default:
			return false;
	}
}

function findInvalidSettingsSemantics(value: unknown): string | null {
	if (!isRecord(value)) return null;
	const media = isRecord(value.media) ? value.media : null;
	const ocr = media && isRecord(media.ocr) ? media.ocr : null;
	if (
		ocr &&
		typeof ocr.provider === 'string' &&
		!['llm', 'baidu', 'azure', 'tencent', 'none'].includes(ocr.provider)
	) {
		return 'settings.media.ocr.provider';
	}

	const llm = isRecord(value.llm) ? value.llm : null;
	const providers = llm && Array.isArray(llm.providers) ? llm.providers : [];
	const models = llm && Array.isArray(llm.models) ? llm.models : [];
	for (const [index, provider] of providers.entries()) {
		if (!isRecord(provider)) continue;
		if (
			provider.default_web_search !== undefined &&
			provider.default_web_search !== null &&
			!isSupportedWebSearchSetting(provider.default_web_search)
		) {
			return `settings.llm.providers[${index}].default_web_search`;
		}
	}
	for (const [index, model] of models.entries()) {
		if (!isRecord(model)) continue;
		if (
			model.web_search !== undefined &&
			model.web_search !== null &&
			!isSupportedWebSearchSetting(model.web_search)
		) {
			return `settings.llm.models[${index}].web_search`;
		}
		if (model.independent_search != null) {
			const provider = providers.find(
				(candidate) => isRecord(candidate) && candidate.name === model.provider_name,
			);
			if (
				!isRecord(provider) ||
				!isIndependentSearchConfig(model.independent_search, provider.provider)
			) {
				return `settings.llm.models[${index}].independent_search`;
			}
		}
	}

	const stt = media && isRecord(media.stt) ? media.stt : null;
	if (stt && typeof stt.provider_name === 'string') {
		const name = stt.provider_name.trim();
		const mode = name.toLowerCase();
		if (!name) return 'settings.media.stt.provider_name';
		if (mode === 'mcp') {
			const serverName = typeof stt.mcp_server === 'string' ? stt.mcp_server.trim() : '';
			const servers = Array.isArray(value.mcp_servers) ? value.mcp_servers : [];
			if (
				!serverName ||
				!servers.some((server) => isRecord(server) && server.name === serverName)
			) {
				return 'settings.media.stt.mcp_server';
			}
		} else if (
			!['llm', 'none'].includes(mode) &&
			!providers.some((provider) => isRecord(provider) && provider.name === name)
		) {
			return 'settings.media.stt.provider_name';
		}
	}
	for (const kind of ['tts', 'image_gen'] as const) {
		const config = media && isRecord(media[kind]) ? media[kind] : null;
		if (config && typeof config.provider_name === 'string') {
			const name = config.provider_name.trim();
			if (
				name &&
				name.toLowerCase() !== 'none' &&
				!providers.some((provider) => isRecord(provider) && provider.name === name)
			) {
				return `settings.media.${kind}.provider_name`;
			}
		}
	}
	return null;
}

/** Reject Settings numbers outside their Rust field domains or JavaScript-safe range. */
export function validateSettingsNumbers(value: unknown): void {
	const invalidPath =
		findInvalidSettingsNumber(value) ??
		findInvalidMcpInterval(value) ??
		findInvalidMediaTimeout(value) ??
		findInvalidSessionStepBudget(value) ??
		findInvalidAudioCapture(value) ??
		findInvalidConcurrency(value) ??
		findInvalidResourceLimits(value) ??
		findInvalidContextWindows(value);
	if (invalidPath) throw new Error(`Invalid Settings numeric value at ${invalidPath}`);
}

/** Validate persisted session identities before Settings consumes permission grants. */
export function parseSessionPermissionGrantResponses(value: unknown): SessionPermissionGrant[] {
	if (!Array.isArray(value)) {
		throw new Error('Invalid session permission grant identity');
	}
	for (const [index, grant] of value.entries()) {
		if (!isRecord(grant) || !isCanonicalEntityId(grant.session_id, 'ses')) {
			throw new Error('Invalid session permission grant identity');
		}
		const invalidPath = findGeneratedWireShapeMismatch(
			grant,
			SESSION_PERMISSION_GRANT_RESPONSE_SHAPE,
		);
		if (invalidPath) {
			throw new Error(
				`Invalid session permission grant at [${index}]${invalidPath.slice(1)}`,
			);
		}
		if (!isCapabilityScope(grant.capability)) {
			throw new Error('Invalid session permission grant identity');
		}
	}
	return (value as GeneratedSessionPermissionGrant[]).map((grant) => {
		const { session_id, ...rest } = grant;
		return { ...rest, sessionId: session_id };
	});
}

/** Validate an opaque credential reference before Settings stores it in its draft. */
export function parseStagedCredentialReference(value: unknown): string {
	if (!isCanonicalEntityId(value, 'cred')) {
		throw new Error('Invalid staged credential reference');
	}
	return value;
}

/** Validate a complete Settings response using the Rust-generated wire shape. */
export function parseSettingsPayload(value: unknown): SettingsPayload | null {
	if (!isRecord(value)) return null;
	if (!hasValidSettingsCredentialReferences(value)) {
		throw new Error('Invalid Settings credential reference');
	}
	validateSettingsNumbers(value);
	const invalidSemanticPath = findInvalidSettingsSemantics(value);
	if (invalidSemanticPath) {
		throw new Error(`Invalid Settings semantic value at ${invalidSemanticPath}`);
	}
	const invalidPath = findGeneratedWireShapeMismatch(value, SETTINGS_RESPONSE_SHAPE);
	if (invalidPath) {
		throw new Error(`Invalid get_settings response shape at ${invalidPath}`);
	}
	return value as unknown as SettingsPayload;
}
