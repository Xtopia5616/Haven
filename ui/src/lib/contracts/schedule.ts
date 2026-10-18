import type { ScheduleMode as GeneratedScheduleMode } from './generatedCommands.ts';
import { SCHEDULE_MODE_VALUES } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf, isString } from './valueGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

export type ScheduleMode = GeneratedScheduleMode;
export type ScheduleType = 'once' | 'recurring_session';

export function isScheduleMode(value: unknown): value is ScheduleMode {
	return isOneOf(value, SCHEDULE_MODE_VALUES);
}

/** Renderer-safe view of an enabled Schedule plan; model inputs never cross this boundary. */
export interface SchedulePlanPayload {
	scheduleId: string;
	ownerSessionId: string;
	scheduleType: ScheduleType;
	title: string;
	body: string;
	mode: ScheduleMode;
	dueAt?: string;
	watchToolRunId?: string;
	intervalSeconds?: number;
	anchorAt?: string;
	nextDueAt?: string;
	activeScheduleRunId?: string;
	createdAt: string;
	updatedAt: string;
}

function isOptionalString(record: Record<string, unknown>, key: string): boolean {
	return record[key] === undefined || isString(record[key]);
}

export function mapSchedulePlan(value: unknown): SchedulePlanPayload | null {
	if (!isRecord(value)) return null;
	if (
		!isCanonicalEntityId(value.schedule_id, 'sched') ||
		!isCanonicalEntityId(value.owner_session_id, 'ses') ||
		!isOneOf(value.schedule_type, ['once', 'recurring_session']) ||
		!isString(value.title) ||
		!isString(value.body) ||
		!isScheduleMode(value.mode) ||
		!isString(value.created_at) ||
		!isString(value.updated_at) ||
		!isOptionalString(value, 'due_at') ||
		!isOptionalString(value, 'anchor_at') ||
		!isOptionalString(value, 'next_due_at')
	) {
		return null;
	}
	if (
		(value.watch_tool_run_id !== undefined &&
			!isCanonicalEntityId(value.watch_tool_run_id, 'toolrun')) ||
		(value.active_schedule_run_id !== undefined &&
			!isCanonicalEntityId(value.active_schedule_run_id, 'schrun')) ||
		(value.interval_seconds !== undefined &&
			(typeof value.interval_seconds !== 'number' ||
				!Number.isInteger(value.interval_seconds) ||
				value.interval_seconds <= 0))
	) {
		return null;
	}
	return {
		scheduleId: value.schedule_id,
		ownerSessionId: value.owner_session_id,
		scheduleType: value.schedule_type,
		title: value.title,
		body: value.body,
		mode: value.mode,
		...(isString(value.due_at) ? { dueAt: value.due_at } : {}),
		...(isString(value.watch_tool_run_id) ? { watchToolRunId: value.watch_tool_run_id } : {}),
		...(typeof value.interval_seconds === 'number'
			? { intervalSeconds: value.interval_seconds }
			: {}),
		...(isString(value.anchor_at) ? { anchorAt: value.anchor_at } : {}),
		...(isString(value.next_due_at) ? { nextDueAt: value.next_due_at } : {}),
		...(isString(value.active_schedule_run_id)
			? { activeScheduleRunId: value.active_schedule_run_id }
			: {}),
		createdAt: value.created_at,
		updatedAt: value.updated_at,
	};
}

export function mapSchedulePlans(value: unknown): SchedulePlanPayload[] {
	return Array.isArray(value)
		? value.map(mapSchedulePlan).filter((row): row is SchedulePlanPayload => row !== null)
		: [];
}
