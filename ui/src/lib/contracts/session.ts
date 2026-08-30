/**
 * Session IPC event contract at the frontend boundary.
 *
 * Tauri serializes Rust payloads with snake_case keys. This module is the one
 * allowed conversion point; Svelte routes receive camelCase fields only.
 */

export const SESSION_EVENT_NAMES = [
	'session:created',
	'session:updated',
	'session:completed',
	'session:error',
	'session:title-updated',
	'session:deleted',
] as const;

export type SessionEventName = (typeof SESSION_EVENT_NAMES)[number];

export interface SessionLifecyclePayload {
	sessionId: string;
	status: string;
	title: string | null;
}

export interface SessionErrorPayload {
	sessionId: string;
	error: string;
}

export interface SessionTitleUpdatedPayload {
	sessionId: string;
	title: string;
}

export interface SessionDeletedPayload {
	/** `null` means `clear_history` removed every session. */
	sessionId: string | null;
}

export interface SessionEventPayloadMap {
	'session:created': SessionLifecyclePayload;
	'session:updated': SessionLifecyclePayload;
	'session:completed': SessionLifecyclePayload;
	'session:error': SessionErrorPayload;
	'session:title-updated': SessionTitleUpdatedPayload;
	'session:deleted': SessionDeletedPayload;
}

interface SessionLifecycleWirePayload {
	session_id: string;
	status: string;
	title: string | null;
}

interface SessionErrorWirePayload {
	session_id: string;
	error: string;
}

interface SessionTitleUpdatedWirePayload {
	session_id: string;
	title: string;
}

interface SessionDeletedWirePayload {
	session_id: string | null;
}

interface SessionWirePayloadMap {
	'session:created': SessionLifecycleWirePayload;
	'session:updated': SessionLifecycleWirePayload;
	'session:completed': SessionLifecycleWirePayload;
	'session:error': SessionErrorWirePayload;
	'session:title-updated': SessionTitleUpdatedWirePayload;
	'session:deleted': SessionDeletedWirePayload;
}

export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}

/** Convert one known session event from the Rust/Tauri wire shape. */
export function mapSessionEvent<K extends SessionEventName>(
	event: TauriEvent<SessionWirePayloadMap[K]>,
): TauriEvent<SessionEventPayloadMap[K]> {
	const payload = event.payload;
	switch (event.event as K) {
		case 'session:created':
		case 'session:updated':
		case 'session:completed':
			return {
				...event,
				payload: {
					sessionId: (payload as SessionLifecycleWirePayload).session_id,
					status: (payload as SessionLifecycleWirePayload).status,
					title: (payload as SessionLifecycleWirePayload).title,
				},
			} as TauriEvent<SessionEventPayloadMap[K]>;
		case 'session:error':
			return {
				...event,
				payload: {
					sessionId: (payload as SessionErrorWirePayload).session_id,
					error: (payload as SessionErrorWirePayload).error,
				},
			} as TauriEvent<SessionEventPayloadMap[K]>;
		case 'session:title-updated':
			return {
				...event,
				payload: {
					sessionId: (payload as SessionTitleUpdatedWirePayload).session_id,
					title: (payload as SessionTitleUpdatedWirePayload).title,
				},
			} as TauriEvent<SessionEventPayloadMap[K]>;
		case 'session:deleted':
			return {
				...event,
				payload: { sessionId: (payload as SessionDeletedWirePayload).session_id },
			} as TauriEvent<SessionEventPayloadMap[K]>;
	}
}
