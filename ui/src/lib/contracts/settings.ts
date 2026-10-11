/** Stable command responses used by the settings diagnostics UI. */

import type {
	ApiKeyStatus as GeneratedApiKeyStatus,
	LogInfo as GeneratedLogInfo,
	LogTail as GeneratedLogTail,
	ShellAvailability as GeneratedShellAvailability,
	Settings as GeneratedSettings,
	SessionPermissionGrant as GeneratedSessionPermissionGrant,
	TauriCommandRequest,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import {
	isBoolean,
	isFiniteNumber,
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
	'top_k',
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

const MAX_INPUT_RING_BUFFER_SECS = 120;

export type LogInfo = GeneratedLogInfo;
export type LogTail = GeneratedLogTail;
export type ShellAvailability = GeneratedShellAvailability;
export type ApiKeyStatus = GeneratedApiKeyStatus;
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

function hasValidSettingsNumbers(value: unknown, field?: string): boolean {
	if (typeof value === 'number') {
		const floatRange = field === undefined ? undefined : FLOAT_RANGES.get(field);
		if (floatRange) {
			return isFiniteNumber(value) && value >= floatRange[0] && value <= floatRange[1];
		}
		if (field !== undefined && FLOAT_NONNEGATIVE_FIELDS.has(field)) {
			return isFiniteNumber(value) && value >= 0;
		}
		if (field !== undefined && U32_FIELDS.has(field)) return isU32(value);
		if (!isSafeUnsignedInteger(value)) return false;
		if (field !== undefined && U16_FIELDS.has(field) && value > 0xffff) return false;
		if (field === 'input_ring_buffer_secs' && value > MAX_INPUT_RING_BUFFER_SECS) return false;
		return true;
	}
	if (Array.isArray(value)) {
		return value.every((item) => hasValidSettingsNumbers(item, field));
	}
	if (isRecord(value)) {
		return Object.entries(value).every(([key, item]) => hasValidSettingsNumbers(item, key));
	}
	return true;
}

function hasValidMcpIntervals(value: unknown): boolean {
	if (!isRecord(value) || !isRecord(value.mcp_discovery)) return true;
	const mcp = value.mcp_discovery;
	if (
		mcp.health_interval_secs !== undefined &&
		(!isSafeUnsignedInteger(mcp.health_interval_secs) || mcp.health_interval_secs === 0)
	) {
		return false;
	}
	if (
		mcp.reconnect_initial_ms !== undefined &&
		(!isSafeUnsignedInteger(mcp.reconnect_initial_ms) || mcp.reconnect_initial_ms === 0)
	) {
		return false;
	}
	if (
		mcp.reconnect_max_ms !== undefined &&
		(!isSafeUnsignedInteger(mcp.reconnect_max_ms) || mcp.reconnect_max_ms === 0)
	) {
		return false;
	}
	return !(
		isSafeUnsignedInteger(mcp.reconnect_initial_ms) &&
		isSafeUnsignedInteger(mcp.reconnect_max_ms) &&
		mcp.reconnect_max_ms < mcp.reconnect_initial_ms
	);
}

/** Reject Settings numbers outside their Rust field domains or JavaScript-safe range. */
export function validateSettingsNumbers(value: unknown): void {
	if (!hasValidSettingsNumbers(value) || !hasValidMcpIntervals(value)) {
		throw new Error('Invalid Settings numeric value');
	}
}

/** Validate persisted session identities before Settings consumes permission grants. */
export function parseSessionPermissionGrantResponses(value: unknown): SessionPermissionGrant[] {
	if (
		!Array.isArray(value) ||
		!value.every((grant) => isRecord(grant) && isCanonicalEntityId(grant.session_id, 'ses'))
	) {
		throw new Error('Invalid session permission grant identity');
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

/**
 * The full Settings shape is owned by haven_common::config::Settings. Keep the
 * renderer shape tied to the generated Rust contract, while malformed root
 * values keep the existing no-op behavior.
 */
export function parseSettingsPayload(value: unknown): SettingsPayload | null {
	if (!isRecord(value)) return null;
	if (!hasValidSettingsCredentialReferences(value)) {
		throw new Error('Invalid Settings credential reference');
	}
	validateSettingsNumbers(value);
	return value as unknown as SettingsPayload;
}

export function parseLogInfo(value: unknown): LogInfo {
	if (
		!isRecord(value) ||
		!isBoolean(value.enabled) ||
		!isString(value.level) ||
		(value.path !== null && !isString(value.path))
	) {
		throw new Error('invalid get_log_info response');
	}
	return { enabled: value.enabled, level: value.level, path: value.path };
}

export function parseLogTail(value: unknown): LogTail {
	if (!isRecord(value) || !isString(value.path) || !isString(value.content)) {
		throw new Error('invalid read_log_tail response');
	}
	return { path: value.path, content: value.content };
}

export function parseShellAvailability(value: unknown): ShellAvailability {
	if (!isRecord(value) || !isBoolean(value.available)) {
		throw new Error('invalid check_shell_available response');
	}
	return { available: value.available };
}

export function parseApiKeyStatus(value: unknown): ApiKeyStatus {
	const requiredFlags = ['stt', 'ocr', 'ocr_secret'] as const;
	if (
		!isRecord(value) ||
		!requiredFlags.every((key) => isBoolean(value[key])) ||
		!isRecord(value.models) ||
		!Object.values(value.models).every(isBoolean) ||
		!isRecord(value.providers) ||
		!Object.values(value.providers).every(isBoolean)
	) {
		throw new Error('invalid get_api_key_status response');
	}
	return {
		models: value.models as Record<string, boolean>,
		providers: value.providers as Record<string, boolean>,
		stt: value.stt as boolean,
		ocr: value.ocr as boolean,
		ocr_secret: value.ocr_secret as boolean,
	};
}
