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
import { isFiniteNumber, isOneOf, isString } from './valueGuards.ts';
import {
	hasOwnWireField,
	nonEmptyStringField,
	readStringField,
	type WireRecord,
} from './wireGuards.ts';

/** Compile-time guard: Rust lifecycle variants and the renderer mapper stay aligned. */
export const SESSION_LIFECYCLE_KINDS = {
	created: true,
	updated: true,
	completed: true,
	error: true,
	title_updated: true,
	deleted: true,
} satisfies Record<GeneratedSessionLifecycleEvent['type'], true>;

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
			status: SessionUpdateStatus;
			waitingReason: SessionWaitingReason | null;
			title: string;
			reason: string | null;
	  }
	| { type: 'completed'; sessionId: string; title: string; reason: string }
	| { type: 'error'; sessionId: string; title: string; error: string }
	| { type: 'title_updated'; sessionId: string; title: string }
	| { type: 'deleted'; sessionId: string | null };

/** Reject malformed lifecycle payloads before any consumer sees them. */
export function mapSessionEvent(
	event: TauriEvent<unknown>,
): TauriEvent<SessionLifecyclePayload> | null {
	if (
		event.event !== SESSION_EVENT_NAMES[0] ||
		!isFiniteNumber(event.id) ||
		!isRecord(event.payload)
	) {
		return null;
	}

	const payload = event.payload;
	const type = readStringField(payload, 'type') as GeneratedSessionLifecycleEvent['type'] | null;
	if (type === null) return null;

	switch (type) {
		case 'created': {
			const sessionId = nonEmptyStringField(payload, 'session_id');
			const status = mapSessionStatus(payload.status);
			const waitingReason = mapWaitingReason(payload.waiting_reason);
			const title = requiredNullableStringField(payload, 'title');
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
			const sessionId = nonEmptyStringField(payload, 'session_id');
			const status = mapUpdateStatus(payload.status);
			const waitingReason = mapWaitingReason(payload.waiting_reason);
			const title = readStringField(payload, 'title');
			const reason = optionalStringField(payload, 'reason');
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
			const sessionId = nonEmptyStringField(payload, 'session_id');
			const title = readStringField(payload, 'title');
			const reason = readStringField(payload, 'reason');
			if (sessionId === null || title === null || reason === null) return null;
			return { ...event, payload: { type, sessionId, title, reason } };
		}
		case 'error': {
			const sessionId = nonEmptyStringField(payload, 'session_id');
			const title = readStringField(payload, 'title');
			const error = readStringField(payload, 'error');
			if (sessionId === null || title === null || error === null) return null;
			return { ...event, payload: { type, sessionId, title, error } };
		}
		case 'title_updated': {
			const sessionId = nonEmptyStringField(payload, 'session_id');
			const title = readStringField(payload, 'title');
			if (sessionId === null || title === null) return null;
			return { ...event, payload: { type, sessionId, title } };
		}
		case 'deleted': {
			const sessionId = requiredNullableStringField(payload, 'session_id');
			if (sessionId === undefined || sessionId === '') return null;
			return { ...event, payload: { type, sessionId } };
		}
		default:
			return null;
	}
}

function mapSessionStatus(value: unknown): SessionStatus | null {
	return isSessionStatus(value) ? value : null;
}

export function isSessionStatus(value: unknown): value is SessionStatus {
	return isOneOf(value, SESSION_STATUS_VALUES);
}

function mapUpdateStatus(value: unknown): SessionUpdateStatus | null {
	return isOneOf(value, SESSION_UPDATE_STATUS_VALUES) ? value : null;
}

function mapWaitingReason(value: unknown): SessionWaitingReason | null | undefined {
	if (value === undefined) return null;
	return isOneOf(value, SESSION_WAITING_REASON_VALUES) ? value : undefined;
}

function requiredNullableStringField(
	payload: WireRecord,
	field: string,
): string | null | undefined {
	if (!hasOwnWireField(payload, field)) return undefined;
	const value = payload[field];
	return value === null || isString(value) ? value : undefined;
}

function optionalStringField(payload: WireRecord, field: string): string | null | undefined {
	if (!hasOwnWireField(payload, field)) return null;
	return isString(payload[field]) ? payload[field] : undefined;
}
