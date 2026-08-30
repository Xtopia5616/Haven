/**
 * Action IPC contract at the frontend boundary.
 *
 * Rust uses snake_case on the Tauri wire.  This module is the only action
 * conversion point; routes and stores use the camelCase record below.
 */
export const ACTION_EVENT_NAMES = [
	'action:created',
	'action:updated',
	'action:output',
	'action:finished',
] as const;

export type ActionEventName = (typeof ACTION_EVENT_NAMES)[number];
export type ActionKind = 'background' | 'scheduled';

export interface ActionPayload {
	id: string;
	kind: ActionKind;
	status?: string;
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

interface ActionWirePayload {
	id: string;
	kind: ActionKind;
	status?: string;
	session_id?: string;
	started_at?: string;
	finished_at?: string;
	due_at?: string;
	title?: string;
	body?: string;
	mode?: string;
	command?: string;
	output?: string;
	error?: string;
	error_reason?: string;
	exit_code?: number;
	preview?: string;
}

export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}

/** Convert a command result or event payload from the Rust wire shape. */
export function mapActionPayload(payload: ActionWirePayload): ActionPayload {
	return {
		id: payload.id,
		kind: payload.kind,
		...(payload.status !== undefined ? { status: payload.status } : {}),
		...(payload.session_id !== undefined ? { sessionId: payload.session_id } : {}),
		...(payload.started_at !== undefined ? { startedAt: payload.started_at } : {}),
		...(payload.finished_at !== undefined ? { finishedAt: payload.finished_at } : {}),
		...(payload.due_at !== undefined ? { dueAt: payload.due_at } : {}),
		...(payload.title !== undefined ? { title: payload.title } : {}),
		...(payload.body !== undefined ? { body: payload.body } : {}),
		...(payload.mode !== undefined ? { mode: payload.mode } : {}),
		...(payload.command !== undefined ? { command: payload.command } : {}),
		...(payload.output !== undefined ? { output: payload.output } : {}),
		...(payload.error !== undefined ? { error: payload.error } : {}),
		...(payload.error_reason !== undefined ? { errorReason: payload.error_reason } : {}),
		...(payload.exit_code !== undefined ? { exitCode: payload.exit_code } : {}),
		...(payload.preview !== undefined ? { preview: payload.preview } : {}),
	};
}

export function mapActionEvent(event: TauriEvent<ActionWirePayload>): TauriEvent<ActionPayload> {
	return { ...event, payload: mapActionPayload(event.payload) };
}
