import type {
	ToolRunKindDto as GeneratedToolRunKindDto,
	ToolRunStatus as GeneratedToolRunStatus,
} from './generatedCommands.ts';
import {
	TOOL_RUN_EVENT_NAMES,
	TOOL_RUN_KIND_DTO_VALUES,
	TOOL_RUN_STATUS_VALUES,
} from './generatedCommands.ts';
import { isValidTauriEventId, type TauriEvent } from './tauriEvent.ts';
import { matchesGeneratedEventPayload } from './generatedWireShape.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf, isString } from './valueGuards.ts';
import {
	canonicalIdField,
	isCanonicalEntityId,
	optionalCanonicalIdFieldIsValid,
	optionalStringFieldIsValid,
	type WireRecord,
} from './wireGuards.ts';

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

export interface ToolRunPayload {
	toolRunId: string;
	kind: ToolRunKind;
	toolName: string;
	status?: ToolRunStatus;
	sessionId?: string;
	sourceStepId?: string;
	startedAt?: string;
	finishedAt?: string;
	output?: string;
	error?: string;
	errorReason?: string;
	preview?: string;
}

/** Full command row projection; command list/history rows always carry status. */
export interface ToolRunCommandRow extends ToolRunPayload {
	status: ToolRunStatus;
}

const OPTIONAL_STRING_FIELDS = [
	'started_at',
	'finished_at',
	'output',
	'error',
	'error_reason',
	'preview',
] as const;

function isToolRunKind(value: unknown): value is ToolRunKind {
	return isOneOf(value, TOOL_RUN_KIND_DTO_VALUES);
}

function isToolName(value: unknown): value is string {
	return isString(value) && value.trim().length > 0 && value.length <= 128;
}

export function isToolRunStatus(value: unknown): value is ToolRunStatus {
	return isOneOf(value, TOOL_RUN_STATUS_VALUES);
}

function hasValidOptionalFields(payload: WireRecord): boolean {
	if (payload.status !== undefined && !isToolRunStatus(payload.status)) {
		return false;
	}
	if (
		!optionalCanonicalIdFieldIsValid(payload, 'session_id', 'ses') ||
		!optionalCanonicalIdFieldIsValid(payload, 'source_step_id', 'step')
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
export function mapToolRunPayload(
	payload: unknown,
	expectedSessionId?: string,
): ToolRunPayload | null {
	if (!isRecord(payload)) return null;
	const toolRunId = canonicalIdField(payload, 'tool_run_id', 'toolrun');
	if (toolRunId === null || !isToolName(payload.tool_name)) return null;
	if (!isToolRunKind(payload.kind)) return null;
	if (!hasValidOptionalFields(payload)) return null;
	if (
		expectedSessionId !== undefined &&
		(!isCanonicalEntityId(expectedSessionId, 'ses') || payload.session_id !== expectedSessionId)
	) {
		return null;
	}

	const mapped: ToolRunPayload = { toolRunId, kind: payload.kind, toolName: payload.tool_name };
	if (isToolRunStatus(payload.status)) mapped.status = payload.status;
	if (isCanonicalEntityId(payload.session_id, 'ses')) mapped.sessionId = payload.session_id;
	if (isCanonicalEntityId(payload.source_step_id, 'step')) {
		mapped.sourceStepId = payload.source_step_id;
	}
	if (isString(payload.started_at)) mapped.startedAt = payload.started_at;
	if (isString(payload.finished_at)) mapped.finishedAt = payload.finished_at;
	if (isString(payload.output)) mapped.output = payload.output;
	if (isString(payload.error)) mapped.error = payload.error;
	if (isString(payload.error_reason)) mapped.errorReason = payload.error_reason;
	if (isString(payload.preview)) mapped.preview = payload.preview;
	return mapped;
}

/** Validate a ToolRun board/history row, whose command projection always has status. */
export function mapToolRunCommandRow(
	payload: unknown,
	expectedSessionId?: string,
): ToolRunCommandRow | null {
	const mapped = mapToolRunPayload(payload, expectedSessionId);
	if (!mapped || mapped.status === undefined) return null;
	return { ...mapped, status: mapped.status };
}

/** Validate the Tauri envelope and map a ToolRun lifecycle event. */
export function mapToolRunEvent(event: TauriEvent<unknown>): TauriEvent<ToolRunPayload> | null {
	if (
		!isRecord(event) ||
		!isOneOf(event.event, TOOL_RUN_EVENT_NAMES) ||
		!isValidTauriEventId(event.id) ||
		!matchesGeneratedEventPayload(event.event, event.payload)
	) {
		return null;
	}
	if (!isRecord(event.payload)) return null;
	const wire = event.payload;
	if (event.event === 'tool_run:created' && wire.status === undefined) return null;
	if (
		event.event === 'tool_run:finished' &&
		!['completed', 'failed', 'cancelled'].includes(String(wire.status))
	) {
		return null;
	}
	const payload = mapToolRunPayload(event.payload);
	return payload ? { ...event, payload } : null;
}
