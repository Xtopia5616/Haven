import { describe, expect, it } from 'vitest';
import { mapSessionEvent } from './session.ts';

describe('mapSessionEvent', () => {
	it('converts lifecycle fields at the frontend boundary', () => {
		const event = mapSessionEvent({
			event: 'session:updated',
			id: 7,
			payload: {
				session_id: 'ses-1',
				status: 'paused',
				waiting_reason: 'ask',
				title: '',
				future_field: 'ignored',
			},
		});

		expect(event).toEqual({
			event: 'session:updated',
			id: 7,
			payload: {
				sessionId: 'ses-1',
				status: 'paused',
				waitingReason: 'ask',
				title: '',
				reason: null,
			},
		});
		expect(event?.payload).not.toHaveProperty('session_id');
		expect(event?.payload).not.toHaveProperty('future_field');
	});

	it('preserves the explicit end retry reason from the backend', () => {
		const event = mapSessionEvent({
			event: 'session:updated',
			id: 13,
			payload: {
				session_id: 'ses-1',
				status: 'paused',
				waiting_reason: 'end_incomplete',
				title: null,
			},
		});

		expect(event?.payload).toEqual({
			sessionId: 'ses-1',
			status: 'paused',
			waitingReason: 'end_incomplete',
			title: null,
			reason: null,
		});
	});

	it('normalizes omitted optional fields to null', () => {
		const event = mapSessionEvent({
			event: 'session:created',
			id: 7,
			payload: { session_id: 'ses-1', status: 'pending', title: null },
		});

		expect(event?.payload).toEqual({
			sessionId: 'ses-1',
			status: 'pending',
			waitingReason: null,
			title: null,
			reason: null,
		});
	});

	it('preserves the global-clear sentinel on session deletion', () => {
		const event = mapSessionEvent({
			event: 'session:deleted',
			id: 8,
			payload: { session_id: null },
		});

		expect(event?.payload).toEqual({ sessionId: null });
	});

	it('maps error and title lifecycle payloads', () => {
		const error = mapSessionEvent({
			event: 'session:error',
			id: 8,
			payload: { session_id: 'ses-1', error: 'sanitized failure', future_field: true },
		});
		const title = mapSessionEvent({
			event: 'session:title-updated',
			id: 9,
			payload: { session_id: 'ses-1', title: 'A title' },
		});

		expect(error?.payload).toEqual({ sessionId: 'ses-1', error: 'sanitized failure' });
		expect(title?.payload).toEqual({ sessionId: 'ses-1', title: 'A title' });
	});

	it('drops unknown event names', () => {
		const event = mapSessionEvent({
			event: 'session:future-event',
			id: 10,
			payload: { session_id: 'ses-1' },
		});

		expect(event).toBeNull();
	});

	it.each([
		{ status: 'running', title: null },
		{ session_id: 7, status: 'running', title: null },
		{ session_id: 'ses-1', status: 'running', title: 7 },
		{ session_id: 'ses-1', status: 'running', title: null, reason: 7 },
	])('drops malformed lifecycle payloads: %o', (payload) => {
		const event = mapSessionEvent({ event: 'session:updated', id: 11, payload });

		expect(event).toBeNull();
	});

	it('rejects unknown lifecycle statuses', () => {
		const event = mapSessionEvent({
			event: 'session:updated',
			id: 9,
			payload: { session_id: 'ses-1', status: 'unknown', title: '' },
		});

		expect(event).toBeNull();
	});

	it('rejects unknown waiting reasons and accepts omitted values', () => {
		const event = mapSessionEvent({
			event: 'session:updated',
			id: 12,
			payload: {
				session_id: 'ses-1',
				status: 'paused',
				waiting_reason: 'future_reason',
				title: null,
			},
		});

		expect(event).toBeNull();
		expect(
			mapSessionEvent({
				event: 'session:updated',
				id: 13,
				payload: { session_id: 'ses-1', status: 'paused', title: null },
			})?.payload.waitingReason,
		).toBeNull();
		expect(
			mapSessionEvent({
				event: 'session:updated',
				id: 14,
				payload: {
					session_id: 'ses-1',
					status: 'paused',
					waiting_reason: null,
					title: null,
				},
			}),
		).toBeNull();
	});

	it('rejects explicit null for an omitted optional reason field', () => {
		expect(
			mapSessionEvent({
				event: 'session:updated',
				id: 15,
				payload: {
					session_id: 'ses-1',
					status: 'paused',
					title: null,
					reason: null,
				},
			}),
		).toBeNull();
	});
});
