import { describe, expect, it } from 'vitest';
import { mapSessionEvent } from './session.ts';

describe('mapSessionEvent', () => {
	it('maps every lifecycle variant through the single event channel', () => {
		const values = [
			{
				type: 'created',
				session_id: 'ses-04f2ae5c9803bd86a8d32aecc861f7cc',
				status: 'pending',
				title: null,
			},
			{
				type: 'updated',
				session_id: 'ses-eda8e43e587f1b8c84315e90d5fe4bb7',
				status: 'paused',
				waiting_reason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				session_id: 'ses-b82d8fd3c1af1e62e95aa0d423f4defb',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				session_id: 'ses-fa042c90a5ae6e4d05dbcaac067fd438',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{
				type: 'title_updated',
				session_id: 'ses-781f5b8fc4beb47521d8a07a2921e919',
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
				sessionId: 'ses-04f2ae5c9803bd86a8d32aecc861f7cc',
				status: 'pending',
				waitingReason: null,
				title: null,
			},
			{
				type: 'updated',
				sessionId: 'ses-eda8e43e587f1b8c84315e90d5fe4bb7',
				status: 'paused',
				waitingReason: 'ask',
				title: 'A title',
				reason: 'Waiting for input',
			},
			{
				type: 'completed',
				sessionId: 'ses-b82d8fd3c1af1e62e95aa0d423f4defb',
				title: 'Finished',
				reason: 'User ended the session',
			},
			{
				type: 'error',
				sessionId: 'ses-fa042c90a5ae6e4d05dbcaac067fd438',
				title: 'Failed',
				error: 'Sanitized failure',
			},
			{
				type: 'title_updated',
				sessionId: 'ses-781f5b8fc4beb47521d8a07a2921e919',
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
					session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
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
						session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
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
						session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
						status: 'paused',
						title: '',
					},
				}),
			).toBeNull();
		}
	});

	it.each([
		{ type: 'completed', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c', title: 'Done' },
		{ type: 'error', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c', title: 'Failed' },
		{
			type: 'updated',
			session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			status: 'running',
			title: 7,
		},
		{ type: 'deleted', session_id: '' },
	])('rejects malformed lifecycle payloads: %o', (payload) => {
		expect(mapSessionEvent({ event: 'session:lifecycle', id: 3, payload })).toBeNull();
	});

	it.each([
		'ses-short',
		'msg-fc16f96c92a9c37a323bb7d0f47a4e22',
		'ses-1',
		'ses-0123456789abcdef0123456789abcdeF',
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
			{ type: 'future', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
			{
				type: 'updated',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				status: 'future',
				title: '',
			},
			{
				type: 'updated',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
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
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				status: 'paused',
				title: '',
				future_field: 'ignored',
			},
		});

		expect(event?.payload).toEqual({
			type: 'updated',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			status: 'paused',
			waitingReason: null,
			title: '',
			reason: null,
		});
		expect(event?.payload).not.toHaveProperty('future_field');
	});
});
