import type {
	ToolRunKindDto as GeneratedToolRunKindDto,
	ToolRunStatus as GeneratedToolRunStatus,
	ScheduleMode as GeneratedScheduleMode,
} from './generatedCommands.ts';
import {
	SCHEDULE_MODE_VALUES,
	TOOL_RUN_EVENT_NAMES,
	TOOL_RUN_KIND_DTO_VALUES,
	TOOL_RUN_STATUS_VALUES,
} from './generatedCommands.ts';
import type { TauriEvent } from './tauriEvent.ts';
import { isRecord } from './objectGuards.ts';
import { isFiniteNumber, isNumber, isOneOf, isString } from './valueGuards.ts';
import { nonEmptyStringField, optionalStringFieldIsValid, type WireRecord } from './wireGuards.ts';

/**
 * ToolRun IPC contract at the frontend boundary.
 *
 * `crates/app-binary/src/events.rs::ToolRunEvent` is the Rust wire authority.
 * This module validates untrusted Tauri/invoke values and is the only ToolRun
 * snake_case-to-camelCase mapping point. Dynamic execution arguments are not
 * part of this UI DTO.
 */
export type ToolRunEventName = (typeof TOOL_RUN_EVENT_NAMES)[number];
export type ToolRunKind = GeneratedToolRunKindDto;
export type ToolRunStatus = GeneratedToolRunStatus;
export type ScheduleMode = GeneratedScheduleMode;

export interface ToolRunPayload {
	toolRunId: string;
	kind: ToolRunKind;
	status?: ToolRunStatus;
	sessionId?: string;
	sourceStepId?: string;
	startedAt?: string;
	finishedAt?: string;
	dueAt?: string;
	title?: string;
	body?: string;
	mode?: ScheduleMode;
	command?: string;
	output?: string;
	error?: string;
	errorReason?: string;
	exitCode?: number;
	preview?: string;
}

const OPTIONAL_STRING_FIELDS = [
	'session_id',
	'source_step_id',
	'started_at',
	'finished_at',
	'due_at',
	'title',
	'body',
	'command',
	'output',
	'error',
	'error_reason',
	'preview',
] as const;

function isToolRunKind(value: unknown): value is ToolRunKind {
	return isOneOf(value, TOOL_RUN_KIND_DTO_VALUES);
}

export function isToolRunStatus(value: unknown): value is ToolRunStatus {
	return isOneOf(value, TOOL_RUN_STATUS_VALUES);
}

export function isScheduleMode(value: unknown): value is ScheduleMode {
	return isOneOf(value, SCHEDULE_MODE_VALUES);
}

function hasValidOptionalFields(payload: WireRecord): boolean {
	if (payload.status !== undefined && !isToolRunStatus(payload.status)) {
		return false;
	}
	if (payload.mode !== undefined && !isScheduleMode(payload.mode)) {
		return false;
	}
	if (
		payload.exit_code !== undefined &&
		(!isNumber(payload.exit_code) ||
			!Number.isInteger(payload.exit_code) ||
			payload.exit_code < -2_147_483_648 ||
			payload.exit_code > 2_147_483_647)
	) {
		return false;
	}
	return OPTIONAL_STRING_FIELDS.every((field) => optionalStringFieldIsValid(payload, field));
}

/**
 * Validate and map a `list_tool_runs` / `list_tool_run_history` ToolRunEvent row.
 * Unknown additive fields are ignored. A missing or invalid required field,
 * including an unknown `kind` discriminator, drops the whole row.
 */
export function mapToolRunPayload(payload: unknown): ToolRunPayload | null {
	if (!isRecord(payload)) return null;
	const toolRunId = nonEmptyStringField(payload, 'tool_run_id');
	if (toolRunId === null) return null;
	if (!isToolRunKind(payload.kind)) return null;
	if (!hasValidOptionalFields(payload)) return null;

	const mapped: ToolRunPayload = { toolRunId, kind: payload.kind };
	if (isToolRunStatus(payload.status)) mapped.status = payload.status;
	if (isString(payload.session_id)) mapped.sessionId = payload.session_id;
	if (isString(payload.source_step_id)) mapped.sourceStepId = payload.source_step_id;
	if (isString(payload.started_at)) mapped.startedAt = payload.started_at;
	if (isString(payload.finished_at)) mapped.finishedAt = payload.finished_at;
	if (isString(payload.due_at)) mapped.dueAt = payload.due_at;
	if (isString(payload.title)) mapped.title = payload.title;
	if (isString(payload.body)) mapped.body = payload.body;
	if (isScheduleMode(payload.mode)) mapped.mode = payload.mode;
	if (isString(payload.command)) mapped.command = payload.command;
	if (isString(payload.output)) mapped.output = payload.output;
	if (isString(payload.error)) mapped.error = payload.error;
	if (isString(payload.error_reason)) mapped.errorReason = payload.error_reason;
	if (isNumber(payload.exit_code)) mapped.exitCode = payload.exit_code;
	if (isString(payload.preview)) mapped.preview = payload.preview;
	return mapped;
}

/** Validate the Tauri envelope and map a ToolRun lifecycle event. */
export function mapToolRunEvent(event: TauriEvent<unknown>): TauriEvent<ToolRunPayload> | null {
	if (
		!isRecord(event) ||
		!isOneOf(event.event, TOOL_RUN_EVENT_NAMES) ||
		!isFiniteNumber(event.id)
	) {
		return null;
	}
	const payload = mapToolRunPayload(event.payload);
	return payload ? { ...event, payload } : null;
}
