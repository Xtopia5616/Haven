import type { MetricsSnapshot as GeneratedMetricsSnapshot } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isSafeUnsignedInteger } from './valueGuards.ts';

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
		!value.phases.every((phase) => hasSafeUnsignedFields(phase, PHASE_FIELDS)) ||
		!hasSafeUnsignedFields(value.counters, COUNTER_FIELDS) ||
		!hasSafeUnsignedFields(value.gauges, GAUGE_FIELDS) ||
		(value.ui !== undefined && !hasSafeUnsignedFields(value.ui, UI_METRIC_FIELDS))
	) {
		throw new Error('Invalid performance metrics response');
	}
	return value as unknown as PerformanceMetricsSnapshot;
}
