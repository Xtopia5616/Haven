import { describe, expect, it } from 'vitest';
import { mapSessionEvent } from './session.ts';

describe('mapSessionEvent', () => {
	it('converts lifecycle fields at the frontend boundary', () => {
		const event = mapSessionEvent({
			event: 'session:updated',
			id: 7,
			payload: { session_id: 'ses-1', status: 'paused', title: '' },
		});

		expect(event).toEqual({
			event: 'session:updated',
			id: 7,
			payload: { sessionId: 'ses-1', status: 'paused', title: '' },
		});
		expect(event.payload).not.toHaveProperty('session_id');
	});

	it('preserves the global-clear sentinel on session deletion', () => {
		const event = mapSessionEvent({
			event: 'session:deleted',
			id: 8,
			payload: { session_id: null },
		});

		expect(event.payload).toEqual({ sessionId: null });
	});
});
