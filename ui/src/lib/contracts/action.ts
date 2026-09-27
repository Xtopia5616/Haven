/**
 * Action IPC contract at the frontend boundary.
 *
 * `crates/app-binary/src/events.rs::ActionEvent` is the Rust wire authority.
 * This module validates untrusted Tauri/invoke values and is the only action
 * snake_case-to-camelCase mapping point. Dynamic execution arguments are not
 * part of this UI DTO.
 */
export const ACTION_EVENT_NAMES = [
	'action:created',
	'action:updated',
	'action:output',
	'action:finished',
] as const;

export type ActionEventName = (typeof ACTION_EVENT_NAMES)[number];
export type ActionKind = 'background' | 'scheduled';
export type ActionStatus = 'waiting' | 'running' | 'completed' | 'failed' | 'cancelled';

const ACTION_STATUSES: readonly ActionStatus[] = [
	'waiting',
	'running',
	'completed',
	'failed',
	'cancelled',
];

export interface ActionPayload {
	id: string;
	kind: ActionKind;
	status?: ActionStatus;
	sessionId?: string;
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

export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}

type WireRecord = Record<string, unknown>;

const OPTIONAL_STRING_FIELDS = [
	'session_id',
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

function isActionKind(value: unknown): value is ActionKind {
	return value === 'background' || value === 'scheduled';
}

function isActionStatus(value: unknown): value is ActionStatus {
	return (ACTION_STATUSES as readonly unknown[]).includes(value);
}

function hasValidOptionalFields(payload: WireRecord): boolean {
	if (payload.status !== undefined && !isActionStatus(payload.status)) {
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
 * Validate and map a `list_actions` / `list_action_history` ActionEvent row.
 * Unknown additive fields are ignored. A missing or invalid required field,
 * including an unknown `kind` discriminator, drops the whole row.
 */
export function mapActionPayload(payload: unknown): ActionPayload | null {
	if (!isRecord(payload)) return null;
	if (typeof payload.id !== 'string' || payload.id.length === 0) return null;
	if (!isActionKind(payload.kind)) return null;
	if (!hasValidOptionalFields(payload)) return null;

	const mapped: ActionPayload = { id: payload.id, kind: payload.kind };
	if (isActionStatus(payload.status)) mapped.status = payload.status;
	if (typeof payload.session_id === 'string') mapped.sessionId = payload.session_id;
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

/** Validate the Tauri envelope and map an action lifecycle event. */
export function mapActionEvent(event: TauriEvent<unknown>): TauriEvent<ActionPayload> | null {
	if (
		!isRecord(event) ||
		typeof event.event !== 'string' ||
		!(ACTION_EVENT_NAMES as readonly string[]).includes(event.event) ||
		typeof event.id !== 'number' ||
		!Number.isFinite(event.id)
	) {
		return null;
	}
	const payload = mapActionPayload(event.payload);
	return payload ? { ...event, payload } : null;
}
