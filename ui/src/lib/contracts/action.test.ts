import { describe, expect, it } from 'vitest';
import { mapActionEvent, mapActionPayload } from './action.ts';

describe('action IPC contract', () => {
	it('maps the stable task wire payload to camelCase without legacy action_id', () => {
		const event = mapActionEvent({
			event: 'action:finished',
			id: 4,
			payload: {
				id: 'act-1',
				kind: 'background',
				status: 'completed',
				session_id: 'ses-1',
				finished_at: '2026-08-26T00:00:00Z',
				exit_code: 0,
			},
		});

		expect(event.payload).toEqual({
			id: 'act-1',
			kind: 'background',
			status: 'completed',
			sessionId: 'ses-1',
			finishedAt: '2026-08-26T00:00:00Z',
			exitCode: 0,
		});
		expect(event.payload).not.toHaveProperty('action_id');
	});

	it('does not create absent optional fields while mapping command results', () => {
		expect(mapActionPayload({ id: 'act-2', kind: 'scheduled' })).toEqual({
			id: 'act-2',
			kind: 'scheduled',
		});
	});
});
