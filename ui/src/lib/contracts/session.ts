/**
 * Session lifecycle IPC contract at the renderer boundary.
 *
 * Rust owns the discriminated wire union. This module validates its unknown
 * runtime payload and maps snake_case fields to the camelCase UI contract.
 */

import {
	SESSION_EVENT_NAMES,
	SESSION_STATUS_VALUES,
	SESSION_UPDATE_STATUS_VALUES,
	SESSION_WAITING_REASON_VALUES,
	type SessionLifecycleEvent as GeneratedSessionLifecycleEvent,
	type SessionStatus,
	type SessionUpdateStatus,
	type SessionWaitingReason,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import type { TauriEvent } from './tauriEvent.ts';

/** Compile-time guard: Rust lifecycle variants and the renderer mapper stay aligned. */
export const SESSION_LIFECYCLE_KINDS = {
	created: true,
	updated: true,
	completed: true,
	error: true,
	title_updated: true,
	deleted: true,
} satisfies Record<GeneratedSessionLifecycleEvent['type'], true>;

type NonTerminalSessionStatus = Extract<SessionStatus, 'pending' | 'running' | 'paused'>;

export type SessionLifecyclePayload =
	| {
			type: 'created';
			sessionId: string;
			status: SessionStatus;
			waitingReason: SessionWaitingReason | null;
			title: string | null;
	  }
	| {
			type: 'updated';
			sessionId: string;
			status: NonTerminalSessionStatus;
			waitingReason: SessionWaitingReason | null;
			title: string;
			reason: string | null;
	  }
	| { type: 'completed'; sessionId: string; title: string; reason: string }
	| { type: 'error'; sessionId: string; title: string; error: string }
	| { type: 'title_updated'; sessionId: string; title: string }
	| { type: 'deleted'; sessionId: string | null };

type SessionWireRecord = Record<string, unknown>;

/** Reject malformed lifecycle payloads before any consumer sees them. */
export function mapSessionEvent(
	event: TauriEvent<unknown>,
): TauriEvent<SessionLifecyclePayload> | null {
	if (
		event.event !== SESSION_EVENT_NAMES[0] ||
		typeof event.id !== 'number' ||
		!Number.isFinite(event.id) ||
		!isRecord(event.payload)
	) {
		return null;
	}

	const payload = event.payload;
	const type = requiredString(payload, 'type') as GeneratedSessionLifecycleEvent['type'] | null;
	if (type === null) return null;

	switch (type) {
		case 'created': {
			const sessionId = requiredSessionId(payload);
			const status = mapSessionStatus(payload.status);
			const waitingReason = mapWaitingReason(payload.waiting_reason);
			const title = nullableString(payload, 'title');
			if (
				sessionId === null ||
				status === null ||
				waitingReason === undefined ||
				title === undefined ||
				(waitingReason !== null && status !== 'paused')
			)
				return null;
			return {
				...event,
				payload: { type, sessionId, status, waitingReason, title },
			};
		}
		case 'updated': {
			const sessionId = requiredSessionId(payload);
			const status = mapUpdateStatus(payload.status);
			const waitingReason = mapWaitingReason(payload.waiting_reason);
			const title = requiredString(payload, 'title');
			const reason = optionalString(payload, 'reason');
			if (
				sessionId === null ||
				status === null ||
				waitingReason === undefined ||
				title === null ||
				reason === undefined ||
				(waitingReason !== null && status !== 'paused')
			)
				return null;
			return {
				...event,
				payload: { type, sessionId, status, waitingReason, title, reason },
			};
		}
		case 'completed': {
			const sessionId = requiredSessionId(payload);
			const title = requiredString(payload, 'title');
			const reason = requiredString(payload, 'reason');
			if (sessionId === null || title === null || reason === null) return null;
			return { ...event, payload: { type, sessionId, title, reason } };
		}
		case 'error': {
			const sessionId = requiredSessionId(payload);
			const title = requiredString(payload, 'title');
			const error = requiredString(payload, 'error');
			if (sessionId === null || title === null || error === null) return null;
			return { ...event, payload: { type, sessionId, title, error } };
		}
		case 'title_updated': {
			const sessionId = requiredSessionId(payload);
			const title = requiredString(payload, 'title');
			if (sessionId === null || title === null) return null;
			return { ...event, payload: { type, sessionId, title } };
		}
		case 'deleted': {
			const sessionId = nullableString(payload, 'session_id');
			if (sessionId === undefined || sessionId === '') return null;
			return { ...event, payload: { type, sessionId } };
		}
		default:
			return null;
	}
}

function mapSessionStatus(value: unknown): SessionStatus | null {
	return (SESSION_STATUS_VALUES as readonly unknown[]).includes(value)
		? (value as SessionStatus)
		: null;
}

function mapUpdateStatus(value: unknown): SessionUpdateStatus | null {
	return (SESSION_UPDATE_STATUS_VALUES as readonly unknown[]).includes(value)
		? (value as SessionUpdateStatus)
		: null;
}

function mapWaitingReason(value: unknown): SessionWaitingReason | null | undefined {
	if (value === undefined) return null;
	return (SESSION_WAITING_REASON_VALUES as readonly unknown[]).includes(value)
		? (value as SessionWaitingReason)
		: undefined;
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
	return typeof payload[field] === 'string' ? payload[field] : undefined;
}
