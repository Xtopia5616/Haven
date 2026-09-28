/**
 * Session IPC event contract at the frontend boundary.
 *
 * Tauri serializes Rust payloads with snake_case keys. This module is the one
 * allowed conversion point; Svelte routes receive camelCase fields only.
 */

import {
	SESSION_STATUSES,
	SESSION_WAITING_REASONS,
	type SessionStatus,
	type SessionWaitingReason,
} from '../sessionStatus.ts';

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
	status: SessionStatus;
	/** Present only when this lifecycle event is paired with a primary terminal channel. */
	occurrenceId?: string;
	waitingReason: SessionWaitingReason | null;
	title: string | null;
	reason: string | null;
}

export interface SessionErrorPayload {
	sessionId: string;
	error: string;
	/** Shared with the matching terminal `session:updated` projection. */
	occurrenceId?: string;
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

export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}

type SessionWireRecord = Record<string, unknown>;

/**
 * Convert a session event from the Rust/Tauri wire shape at the listener
 * boundary. Unknown additive fields are ignored; malformed required fields
 * return `null` so no consumer observes a partial DTO.
 */
export function mapSessionEvent<K extends SessionEventName>(
	event: TauriEvent<unknown> & { event: K },
): TauriEvent<SessionEventPayloadMap[K]> | null;
export function mapSessionEvent(
	event: TauriEvent<unknown>,
): TauriEvent<SessionEventPayloadMap[SessionEventName]> | null;
export function mapSessionEvent<K extends SessionEventName>(
	event: TauriEvent<unknown>,
): TauriEvent<SessionEventPayloadMap[SessionEventName]> | null {
	if (
		typeof event.event !== 'string' ||
		typeof event.id !== 'number' ||
		!Number.isFinite(event.id) ||
		!isRecord(event.payload)
	) {
		return null;
	}

	const payload = event.payload;
	switch (event.event) {
		case 'session:created':
		case 'session:updated':
		case 'session:completed': {
			const sessionId = requiredSessionId(payload);
			const status = mapSessionStatus(payload.status);
			const waitingReason = mapWaitingReason(payload.waiting_reason);
			const title = nullableString(payload, 'title');
			const reason = optionalString(payload, 'reason');
			const occurrenceId = optionalString(payload, 'occurrence_id');
			if (
				sessionId === null ||
				status === null ||
				waitingReason === undefined ||
				title === undefined ||
				reason === undefined ||
				occurrenceId === undefined ||
				occurrenceId === ''
			)
				return null;
			if (
				occurrenceId !== null &&
				!(
					(event.event === 'session:completed' && status === 'completed') ||
					(event.event === 'session:updated' &&
						(status === 'completed' || status === 'error'))
				)
			)
				return null;
			return {
				...event,
				payload: {
					sessionId,
					status,
					waitingReason,
					title,
					reason,
					...(occurrenceId !== null ? { occurrenceId } : {}),
				},
			};
		}
		case 'session:error': {
			const sessionId = requiredSessionId(payload);
			const error = requiredString(payload, 'error');
			const occurrenceId = optionalString(payload, 'occurrence_id');
			if (
				sessionId === null ||
				error === null ||
				occurrenceId === undefined ||
				occurrenceId === ''
			)
				return null;
			return {
				...event,
				payload: { sessionId, error, ...(occurrenceId !== null ? { occurrenceId } : {}) },
			};
		}
		case 'session:title-updated': {
			const sessionId = requiredSessionId(payload);
			const title = requiredString(payload, 'title');
			if (sessionId === null || title === null) return null;
			return {
				...event,
				payload: { sessionId, title },
			};
		}
		case 'session:deleted': {
			const sessionId = nullableString(payload, 'session_id');
			if (sessionId === undefined || sessionId === '') return null;
			return {
				...event,
				payload: { sessionId },
			};
		}
		default:
			return null;
	}
}

function mapSessionStatus(value: unknown): SessionStatus | null {
	return (SESSION_STATUSES as readonly string[]).includes(value as string)
		? (value as SessionStatus)
		: null;
}

function mapWaitingReason(value: unknown): SessionWaitingReason | null | undefined {
	if (value === undefined) return null;
	return (SESSION_WAITING_REASONS as readonly string[]).includes(value as string)
		? (value as SessionWaitingReason)
		: undefined;
}

function isRecord(value: unknown): value is SessionWireRecord {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function requiredSessionId(payload: SessionWireRecord): string | null {
	const value = payload.session_id;
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function requiredString(payload: SessionWireRecord, field: string): string | null {
	const value = payload[field];
	return typeof value === 'string' ? value : null;
}

function nullableString(payload: SessionWireRecord, field: string): string | null | undefined {
	if (!Object.prototype.hasOwnProperty.call(payload, field)) return undefined;
	const value = payload[field];
	return value === null || typeof value === 'string' ? value : undefined;
}

function optionalString(payload: SessionWireRecord, field: string): string | null | undefined {
	if (!Object.prototype.hasOwnProperty.call(payload, field)) return null;
	return typeof payload[field] === 'string' ? (payload[field] as string) : undefined;
}
