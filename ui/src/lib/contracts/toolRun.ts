import type {
	ToolRunKindDto as GeneratedToolRunKindDto,
	ToolRunStatus as GeneratedToolRunStatus,
} from './generatedCommands.ts';
import { TOOL_RUN_KIND_DTO_VALUES, TOOL_RUN_STATUS_VALUES } from './generatedCommands.ts';
import type { TauriEvent } from './tauriEvent.ts';

/**
 * ToolRun IPC contract at the frontend boundary.
 *
 * `crates/app-binary/src/events.rs::ToolRunEvent` is the Rust wire authority.
 * This module validates untrusted Tauri/invoke values and is the only ToolRun
 * snake_case-to-camelCase mapping point. Dynamic execution arguments are not
 * part of this UI DTO.
 */
export const TOOL_RUN_EVENT_NAMES = [
	'tool_run:created',
	'tool_run:updated',
	'tool_run:output',
	'tool_run:finished',
] as const;

export type ToolRunEventName = (typeof TOOL_RUN_EVENT_NAMES)[number];
export type ToolRunKind = GeneratedToolRunKindDto;
export type ToolRunStatus = GeneratedToolRunStatus;

export interface ToolRunPayload {
	id: string;
	kind: ToolRunKind;
	status?: ToolRunStatus;
	sessionId?: string;
	sourceStepId?: string;
	startedAt?: string;
	finishedAt?: string;
	dueAt?: string;
	title?: string;
	body?: string;
	mode?: string;
	command?: string;
	output?: string;
	error?: string;
	errorReason?: string;
	exitCode?: number;
	preview?: string;
}

type WireRecord = Record<string, unknown>;

const OPTIONAL_STRING_FIELDS = [
	'session_id',
	'source_step_id',
	'started_at',
	'finished_at',
	'due_at',
	'title',
	'body',
	'mode',
	'command',
	'output',
	'error',
	'error_reason',
	'preview',
] as const;

function isRecord(value: unknown): value is WireRecord {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isToolRunKind(value: unknown): value is ToolRunKind {
	return (TOOL_RUN_KIND_DTO_VALUES as readonly unknown[]).includes(value);
}

function isToolRunStatus(value: unknown): value is ToolRunStatus {
	return (TOOL_RUN_STATUS_VALUES as readonly unknown[]).includes(value);
}

function hasValidOptionalFields(payload: WireRecord): boolean {
	if (payload.status !== undefined && !isToolRunStatus(payload.status)) {
		return false;
	}
	if (
		payload.exit_code !== undefined &&
		(typeof payload.exit_code !== 'number' ||
			!Number.isInteger(payload.exit_code) ||
			payload.exit_code < -2_147_483_648 ||
			payload.exit_code > 2_147_483_647)
	) {
		return false;
	}
	return OPTIONAL_STRING_FIELDS.every((field) => {
		const value = payload[field];
		return value === undefined || typeof value === 'string';
	});
}

/**
 * Validate and map a `list_tool_runs` / `list_tool_run_history` ToolRunEvent row.
 * Unknown additive fields are ignored. A missing or invalid required field,
 * including an unknown `kind` discriminator, drops the whole row.
 */
export function mapToolRunPayload(payload: unknown): ToolRunPayload | null {
	if (!isRecord(payload)) return null;
	if (typeof payload.id !== 'string' || payload.id.length === 0) return null;
	if (!isToolRunKind(payload.kind)) return null;
	if (!hasValidOptionalFields(payload)) return null;

	const mapped: ToolRunPayload = { id: payload.id, kind: payload.kind };
	if (isToolRunStatus(payload.status)) mapped.status = payload.status;
	if (typeof payload.session_id === 'string') mapped.sessionId = payload.session_id;
	if (typeof payload.source_step_id === 'string') mapped.sourceStepId = payload.source_step_id;
	if (typeof payload.started_at === 'string') mapped.startedAt = payload.started_at;
	if (typeof payload.finished_at === 'string') mapped.finishedAt = payload.finished_at;
	if (typeof payload.due_at === 'string') mapped.dueAt = payload.due_at;
	if (typeof payload.title === 'string') mapped.title = payload.title;
	if (typeof payload.body === 'string') mapped.body = payload.body;
	if (typeof payload.mode === 'string') mapped.mode = payload.mode;
	if (typeof payload.command === 'string') mapped.command = payload.command;
	if (typeof payload.output === 'string') mapped.output = payload.output;
	if (typeof payload.error === 'string') mapped.error = payload.error;
	if (typeof payload.error_reason === 'string') mapped.errorReason = payload.error_reason;
	if (typeof payload.exit_code === 'number') mapped.exitCode = payload.exit_code;
	if (typeof payload.preview === 'string') mapped.preview = payload.preview;
	return mapped;
}

/** Validate the Tauri envelope and map a ToolRun lifecycle event. */
export function mapToolRunEvent(event: TauriEvent<unknown>): TauriEvent<ToolRunPayload> | null {
	if (
		!isRecord(event) ||
		typeof event.event !== 'string' ||
		!(TOOL_RUN_EVENT_NAMES as readonly string[]).includes(event.event) ||
		typeof event.id !== 'number' ||
		!Number.isFinite(event.id)
	) {
		return null;
	}
	const payload = mapToolRunPayload(event.payload);
	return payload ? { ...event, payload } : null;
}
