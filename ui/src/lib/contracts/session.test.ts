import { describe, expect, it } from 'vitest';
import { mapSessionEvent } from './session.ts';

describe('mapSessionEvent', () => {
	it('maps every lifecycle variant through the single event channel', () => {
		const values = [
			{
				type: 'created',
				session_id: 'ses-created',
				status: 'pending',
				title: null,
			},
			{
				type: 'updated',
				session_id: 'ses-updated',
				status: 'paused',
				waiting_reason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				session_id: 'ses-completed',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				session_id: 'ses-error',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{ type: 'title_updated', session_id: 'ses-title', title: 'New title' },
			{ type: 'deleted', session_id: null },
		];

		const mapped = values.map((payload, id) =>
			mapSessionEvent({ event: 'session:lifecycle', id, payload }),
		);

		expect(mapped.map((event) => event?.payload)).toEqual([
			{
				type: 'created',
				sessionId: 'ses-created',
				status: 'pending',
				waitingReason: null,
				title: null,
			},
			{
				type: 'updated',
				sessionId: 'ses-updated',
				status: 'paused',
				waitingReason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				sessionId: 'ses-completed',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				sessionId: 'ses-error',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{ type: 'title_updated', sessionId: 'ses-title', title: 'New title' },
			{ type: 'deleted', sessionId: null },
		]);
	});

	it('rejects an old channel even when the payload is otherwise valid', () => {
		expect(
			mapSessionEvent({
				event: 'session:updated',
				id: 1,
				payload: { type: 'updated', session_id: 'ses-1', status: 'running', title: '' },
			}),
		).toBeNull();
	});

	it('rejects terminal states disguised as ordinary status updates', () => {
		for (const status of ['completed', 'error']) {
			expect(
				mapSessionEvent({
					event: 'session:lifecycle',
					id: 2,
					payload: { type: 'updated', session_id: 'ses-1', status, title: 'A title' },
				}),
			).toBeNull();
		}
	});

	it.each([
		{ type: 'completed', session_id: 'ses-1', title: 'Done' },
		{ type: 'error', session_id: 'ses-1', title: 'Failed' },
		{ type: 'updated', session_id: 'ses-1', status: 'running', title: 7 },
		{ type: 'deleted', session_id: '' },
	])('rejects malformed lifecycle payloads: %o', (payload) => {
		expect(mapSessionEvent({ event: 'session:lifecycle', id: 3, payload })).toBeNull();
	});

	it('rejects unknown variants, statuses, and waiting reasons', () => {
		for (const payload of [
			{ type: 'future', session_id: 'ses-1' },
			{ type: 'updated', session_id: 'ses-1', status: 'future', title: '' },
			{
				type: 'updated',
				session_id: 'ses-1',
				status: 'paused',
				waiting_reason: 'future',
				title: '',
			},
		]) {
			expect(mapSessionEvent({ event: 'session:lifecycle', id: 4, payload })).toBeNull();
		}
	});

	it('does not leak unknown wire fields into the UI payload', () => {
		const event = mapSessionEvent({
			event: 'session:lifecycle',
			id: 5,
			payload: {
				type: 'updated',
				session_id: 'ses-1',
				status: 'paused',
				title: '',
				future_field: 'ignored',
			},
		});

		expect(event?.payload).toEqual({
			type: 'updated',
			sessionId: 'ses-1',
			status: 'paused',
			waitingReason: null,
			title: '',
			reason: null,
		});
		expect(event?.payload).not.toHaveProperty('future_field');
	});
});
