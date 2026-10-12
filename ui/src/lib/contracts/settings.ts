/** Settings request and response contracts owned by the Settings UI boundary. */

import type {
	Settings as GeneratedSettings,
	SessionPermissionGrant as GeneratedSessionPermissionGrant,
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
	isFiniteNumber,
	isOneOf,
	isSafeUnsignedInteger,
	isString,
	isU32,
} from './valueGuards.ts';
import { hasOwnWireField, isCanonicalEntityId } from './wireGuards.ts';

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
	'sample_rate',
	'compaction_reserve_tokens',
]);

const U16_FIELDS = new Set(['bits_per_sample', 'channels']);

const FLOAT_RANGES = new Map<string, readonly [number, number]>([
	['attachment_image_jpeg_quality', [0, 1]],
	['compaction_ratio', [0.1, 0.95]],
	['default_temperature', [0, 2]],
	['min_confidence', [0, 1]],
	['retry_jitter', [0, 1]],
	['temperature', [0, 2]],
	['vad_threshold', [0, 1]],
]);

const FLOAT_NONNEGATIVE_FIELDS = new Set([
	'cost_per_1k_cache_read_tokens',
	'cost_per_1k_cache_write_tokens',
	'cost_per_1k_input_tokens',
	'cost_per_1k_output_tokens',
]);

// Keep this list aligned with the numeric properties in generated Settings
// and SettingsInput. Checking by key catches strings/objects that Rust's typed
// IPC decoder would reject only after the renderer has invoked the command.
const NUMERIC_FIELDS = new Set([
	'attachment_image_jpeg_quality',
	'background_tool_run_output_emit_interval_ms',
	'background_tool_run_tail_max_chars',
	'bits_per_sample',
	'channels',
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
	'cpu_time_secs',
	'default_context_window',
	'default_max_tokens',
	'default_temperature',
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
	'max_memory_mb',
	'max_observation_chars',
	'max_output_chars',
	'max_output_lines',
	'max_pending_scheduled_tool_runs',
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
	'sample_rate',
	'sanitize_field_max_chars',
	'scheduled_tool_runs_due_horizon_secs',
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
	'temperature',
	'timeout_secs',
	'tool_run_result_context_chars',
	'tool_run_terminal_ttl_secs',
	'turn_deadline_secs',
	'vad_threshold',
]);

const NULLABLE_NUMERIC_FIELDS = new Set([
	'cpu_time_secs',
	'context_window',
	'cost_per_1k_cache_read_tokens',
	'cost_per_1k_cache_write_tokens',
	'cost_per_1k_input_tokens',
	'cost_per_1k_output_tokens',
	'default_max_tokens',
	'default_temperature',
	'default_timeout_secs',
	'default_timeout_streaming_secs',
	'max_memory_mb',
	'max_steps_per_session',
	'max_tokens',
	'max_output_chars',
	'max_retries',
	'retry_backoff_secs',
]);

const MAX_INPUT_RING_BUFFER_SECS = 120;

export type SessionPermissionGrant = GeneratedSessionPermissionGrant;

/** Exact Rust-owned config shape from the generated IPC contract. */
export type SettingsPayload = GeneratedSettings;

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
		if (field !== undefined && U16_FIELDS.has(field) && value > 0xffff) return path;
		if (field === 'input_ring_buffer_secs' && value > MAX_INPUT_RING_BUFFER_SECS) return path;
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
	if (field === 'temperature') return path.startsWith('settings.llm.models[');
	if (field === 'reasoning_echo_max_chars') return path.startsWith('settings.llm.models[');
	if (field === 'timeout_secs') return path.startsWith('settings.tool_settings.');
	return false;
}

function findInvalidMcpInterval(value: unknown): string | null {
	if (!isRecord(value) || !isRecord(value.mcp_discovery)) return null;
	const mcp = value.mcp_discovery;
	if (
		mcp.health_interval_secs !== undefined &&
		(!isSafeUnsignedInteger(mcp.health_interval_secs) || mcp.health_interval_secs === 0)
	) {
		return 'settings.mcp_discovery.health_interval_secs';
	}
	if (
		mcp.reconnect_initial_ms !== undefined &&
		(!isSafeUnsignedInteger(mcp.reconnect_initial_ms) || mcp.reconnect_initial_ms === 0)
	) {
		return 'settings.mcp_discovery.reconnect_initial_ms';
	}
	if (
		mcp.reconnect_max_ms !== undefined &&
		(!isSafeUnsignedInteger(mcp.reconnect_max_ms) || mcp.reconnect_max_ms === 0)
	) {
		return 'settings.mcp_discovery.reconnect_max_ms';
	}
	return isSafeUnsignedInteger(mcp.reconnect_initial_ms) &&
		isSafeUnsignedInteger(mcp.reconnect_max_ms) &&
		mcp.reconnect_max_ms < mcp.reconnect_initial_ms
		? 'settings.mcp_discovery.reconnect_max_ms'
		: null;
}

/** Reject Settings numbers outside their Rust field domains or JavaScript-safe range. */
export function validateSettingsNumbers(value: unknown): void {
	const invalidPath = findInvalidSettingsNumber(value) ?? findInvalidMcpInterval(value);
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
			throw new Error(`Invalid session permission grant at [${index}]${invalidPath.slice(1)}`);
		}
	}
	return value as unknown as SessionPermissionGrant[];
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
	const invalidPath = findGeneratedWireShapeMismatch(value, SETTINGS_RESPONSE_SHAPE);
	if (invalidPath) {
		throw new Error(`Invalid get_settings response shape at ${invalidPath}`);
	}
	return value as unknown as SettingsPayload;
}
