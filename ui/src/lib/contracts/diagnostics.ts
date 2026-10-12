import {
	AGENT_METRICS_PHASE_COUNT,
	LOG_LEVEL_VALUES,
	type ApiKeyStatus as GeneratedApiKeyStatus,
	type LogInfo as GeneratedLogInfo,
	type LogTail as GeneratedLogTail,
	type MetricsSnapshot as GeneratedMetricsSnapshot,
	type ShellAvailability as GeneratedShellAvailability,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isBoolean, isOneOf, isSafeUnsignedInteger, isString } from './valueGuards.ts';

export type LogInfo = GeneratedLogInfo;
export type LogTail = GeneratedLogTail;
export type ShellAvailability = GeneratedShellAvailability;
export type ApiKeyStatus = GeneratedApiKeyStatus;
/** Named UI boundary for the complete Rust-owned performance metrics response. */
export type PerformanceMetricsSnapshot = GeneratedMetricsSnapshot;

const PHASE_FIELDS = ['count', 'total_ms', 'p50_ms', 'p95_ms'] as const;
const COUNTER_FIELDS = [
	'turn_starts',
	'first_tokens',
	'stream_chunks',
	'chunk_drops',
	'checkpoint_pending',
	'branch_point_failures',
	'snapshot_failures',
	'projection_failures',
	'inbox_ack_failures',
	'tool_run_result_retries',
	'tool_run_result_duplicates',
	'web_search_drops',
] as const;
const GAUGE_FIELDS = ['context_queue_items'] as const;
const UI_METRIC_FIELDS = ['frames', 'chunks', 'drops'] as const;

function hasSafeUnsignedFields<const Fields extends readonly string[]>(
	value: unknown,
	fields: Fields,
): value is Record<Fields[number], number> {
	return isRecord(value) && fields.every((field) => isSafeUnsignedInteger(value[field]));
}

/**
 * Validate every Rust u64 that crosses JSON as a JavaScript number while
 * returning the original object so export extensions remain intact.
 */
export function parsePerformanceMetricsSnapshot(value: unknown): PerformanceMetricsSnapshot {
	if (
		!isRecord(value) ||
		!Array.isArray(value.phases) ||
		value.phases.length !== AGENT_METRICS_PHASE_COUNT ||
		!value.phases.every((phase) => hasSafeUnsignedFields(phase, PHASE_FIELDS)) ||
		!hasSafeUnsignedFields(value.counters, COUNTER_FIELDS) ||
		!hasSafeUnsignedFields(value.gauges, GAUGE_FIELDS) ||
		(value.ui !== undefined && !hasSafeUnsignedFields(value.ui, UI_METRIC_FIELDS))
	) {
		throw new Error('Invalid performance metrics response');
	}
	return value as unknown as PerformanceMetricsSnapshot;
}

export function parseLogInfo(value: unknown): LogInfo {
	if (
		!isRecord(value) ||
		!isBoolean(value.enabled) ||
		!isOneOf(value.level, LOG_LEVEL_VALUES) ||
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
