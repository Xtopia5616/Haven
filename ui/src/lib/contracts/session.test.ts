import { describe, expect, it } from 'vitest';
import { mapSessionEvent } from './session.ts';

describe('mapSessionEvent', () => {
	it('maps every lifecycle variant through the single event channel', () => {
		const values = [
			{
				type: 'created',
				session_id: 'ses-406effb1e9c59672c66a598c2b21e331',
				status: 'pending',
				title: null,
			},
			{
				type: 'updated',
				session_id: 'ses-27eb5e51506c911f6fc4bb345c0d9db6',
				status: 'paused',
				waiting_reason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				session_id: 'ses-4ddb3e96801a1ee2b77dc5247c0db478',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				session_id: 'ses-ca00fccfb408989eddc401062c4d1219',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{
				type: 'title_updated',
				session_id: 'ses-aaf2320646108059a87ab5017a86aee4',
				title: 'New title',
			},
			{ type: 'deleted', session_id: null },
		];

		const mapped = values.map((payload, id) =>
			mapSessionEvent({ event: 'session:lifecycle', id, payload }),
		);

		expect(mapped.map((event) => event?.payload)).toEqual([
			{
				type: 'created',
				sessionId: 'ses-406effb1e9c59672c66a598c2b21e331',
				status: 'pending',
				waitingReason: null,
				title: null,
			},
			{
				type: 'updated',
				sessionId: 'ses-27eb5e51506c911f6fc4bb345c0d9db6',
				status: 'paused',
				waitingReason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				sessionId: 'ses-4ddb3e96801a1ee2b77dc5247c0db478',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				sessionId: 'ses-ca00fccfb408989eddc401062c4d1219',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{
				type: 'title_updated',
				sessionId: 'ses-aaf2320646108059a87ab5017a86aee4',
				title: 'New title',
			},
			{ type: 'deleted', sessionId: null },
		]);
	});

	it('rejects an old channel even when the payload is otherwise valid', () => {
		expect(
			mapSessionEvent({
				event: 'session:updated',
				id: 1,
				payload: {
					type: 'updated',
					session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
					status: 'running',
					title: '',
				},
			}),
		).toBeNull();
	});

	it('rejects terminal states disguised as ordinary status updates', () => {
		for (const status of ['completed', 'error']) {
			expect(
				mapSessionEvent({
					event: 'session:lifecycle',
					id: 2,
					payload: {
						type: 'updated',
						session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
						status,
						title: 'A title',
					},
				}),
			).toBeNull();
		}
	});

	it('rejects invalid Tauri envelope ids consistently', () => {
		for (const id of [Number.NaN, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
			expect(
				mapSessionEvent({
					event: 'session:lifecycle',
					id,
					payload: {
						type: 'updated',
						session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
						status: 'paused',
						title: '',
					},
				}),
			).toBeNull();
		}
	});

	it.each([
		{ type: 'completed', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57', title: 'Done' },
		{ type: 'error', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57', title: 'Failed' },
		{
			type: 'updated',
			session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			status: 'running',
			title: 7,
		},
		{ type: 'deleted', session_id: '' },
	])('rejects malformed lifecycle payloads: %o', (payload) => {
		expect(mapSessionEvent({ event: 'session:lifecycle', id: 3, payload })).toBeNull();
	});

	it.each([
		'ses-short',
		'msg-0123456789abcdef0123456789abcdef',
		'ses-01234567-89ab-cdef-0123-456789abcdef',
		'ses-0123456789ABCDEF0123456789abcdef',
	])('rejects a non-canonical session identity: %s', (session_id) => {
		expect(
			mapSessionEvent({
				event: 'session:lifecycle',
				id: 9,
				payload: { type: 'updated', session_id, status: 'paused', title: '' },
			}),
		).toBeNull();
	});

	it('rejects unknown variants, statuses, and waiting reasons', () => {
		for (const payload of [
			{ type: 'future', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
			{
				type: 'updated',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				status: 'future',
				title: '',
			},
			{
				type: 'updated',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
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
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				status: 'paused',
				title: '',
				future_field: 'ignored',
			},
		});

		expect(event?.payload).toEqual({
			type: 'updated',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			status: 'paused',
			waitingReason: null,
			title: '',
			reason: null,
		});
		expect(event?.payload).not.toHaveProperty('future_field');
	});
});
