import { describe, expect, it } from 'vitest';
import { mapActionEvent, mapActionPayload } from './action.ts';

describe('action IPC contract', () => {
	it('maps a normal Rust ActionEvent to the UI DTO', () => {
		expect(
			mapActionEvent({
				event: 'action:finished',
				id: 4,
				payload: {
					id: 'act-1',
					kind: 'background',
					status: 'completed',
					session_id: 'ses-1',
					finished_at: '2026-08-26T00:00:00Z',
					exit_code: 0,
					future_wire_field: 'ignored',
				},
			}),
		).toEqual({
			event: 'action:finished',
			id: 4,
			payload: {
				id: 'act-1',
				kind: 'background',
				status: 'completed',
				sessionId: 'ses-1',
				finishedAt: '2026-08-26T00:00:00Z',
				exitCode: 0,
			},
		});
	});

	it('keeps omitted optional fields absent and rejects explicit nulls', () => {
		expect(mapActionPayload({ id: 'act-2', kind: 'scheduled' })).toEqual({
			id: 'act-2',
			kind: 'scheduled',
		});
		expect(
			mapActionPayload({ id: 'act-2', kind: 'scheduled', session_id: null, status: null }),
		).toBeNull();
		expect(mapActionPayload({ id: 'act-2', kind: 'scheduled', exit_code: null })).toBeNull();
	});

	it('rejects unknown statuses and action kinds', () => {
		expect(
			mapActionPayload({ id: 'act-3', kind: 'background', status: 'unexpected' }),
		).toBeNull();
		expect(
			mapActionPayload({ id: 'act-4', kind: 'future-kind', status: 'running' }),
		).toBeNull();
	});

	it('fails closed when required fields are missing or malformed', () => {
		expect(mapActionPayload({ kind: 'background' })).toBeNull();
		expect(mapActionPayload({ id: '', kind: 'background' })).toBeNull();
		expect(mapActionPayload({ id: 'act-5' })).toBeNull();
		expect(mapActionPayload({ id: 'act-6', kind: 'background', session_id: 42 })).toBeNull();
		expect(
			mapActionEvent({ event: 'action:finished', id: 4, payload: { id: 'act-7' } }),
		).toBeNull();
	});

	it('does not transform dynamic tool_args into a board field', () => {
		const toolArgs = { query: { tags: ['private', 'nested'], limit: 3 } };
		const wire = { id: 'act-8', kind: 'scheduled', tool_args: toolArgs };

		// ActionEvent intentionally excludes execution arguments. Keep the JSON
		// extension opaque to this UI DTO mapper rather than stringifying it.
		expect(mapActionPayload(wire)).toEqual({ id: 'act-8', kind: 'scheduled' });
		expect(wire.tool_args).toBe(toolArgs);
		expect(wire.tool_args).toEqual({ query: { tags: ['private', 'nested'], limit: 3 } });
	});
});
