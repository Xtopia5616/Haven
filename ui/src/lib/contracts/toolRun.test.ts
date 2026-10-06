import { describe, expect, it } from 'vitest';
import { mapToolRunEvent, mapToolRunPayload } from './toolRun.ts';

describe('tool run IPC contract', () => {
	it('maps a normal Rust ToolRunEvent to the UI DTO', () => {
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: {
					id: 'toolrun-1',
					kind: 'background',
					status: 'completed',
					session_id: 'ses-1',
					source_step_id: 'step-1',
					finished_at: '2026-08-26T00:00:00Z',
					exit_code: 0,
					future_wire_field: 'ignored',
				},
			}),
		).toEqual({
			event: 'tool_run:finished',
			id: 4,
			payload: {
				id: 'toolrun-1',
				kind: 'background',
				status: 'completed',
				sessionId: 'ses-1',
				sourceStepId: 'step-1',
				finishedAt: '2026-08-26T00:00:00Z',
				exitCode: 0,
			},
		});
	});

	it('keeps omitted optional fields absent and rejects explicit nulls', () => {
		expect(mapToolRunPayload({ id: 'toolrun-2', kind: 'scheduled' })).toEqual({
			id: 'toolrun-2',
			kind: 'scheduled',
		});
		expect(
			mapToolRunPayload({ id: 'toolrun-2', kind: 'scheduled', session_id: null, status: null }),
		).toBeNull();
		expect(mapToolRunPayload({ id: 'toolrun-2', kind: 'scheduled', exit_code: null })).toBeNull();
		expect(
			mapToolRunPayload({ id: 'toolrun-2', kind: 'background', source_step_id: 42 }),
		).toBeNull();
	});

	it('rejects unknown ToolRun statuses and kinds', () => {
		expect(
			mapToolRunPayload({ id: 'toolrun-3', kind: 'background', status: 'unexpected' }),
		).toBeNull();
		expect(
			mapToolRunPayload({ id: 'toolrun-4', kind: 'future-kind', status: 'running' }),
		).toBeNull();
	});

	it('fails closed when required fields are missing or malformed', () => {
		expect(mapToolRunPayload({ kind: 'background' })).toBeNull();
		expect(mapToolRunPayload({ id: '', kind: 'background' })).toBeNull();
		expect(mapToolRunPayload({ id: 'toolrun-5' })).toBeNull();
		expect(mapToolRunPayload({ id: 'toolrun-6', kind: 'background', session_id: 42 })).toBeNull();
		expect(
			mapToolRunEvent({ event: 'tool_run:finished', id: 4, payload: { id: 'toolrun-7' } }),
		).toBeNull();
	});

	it('does not transform dynamic tool_args into a board field', () => {
		const toolArgs = { query: { tags: ['private', 'nested'], limit: 3 } };
		const wire = { id: 'toolrun-8', kind: 'scheduled', tool_args: toolArgs };

		// ToolRunEvent intentionally excludes execution arguments. Keep the JSON
		// extension opaque to this UI DTO mapper rather than stringifying it.
		expect(mapToolRunPayload(wire)).toEqual({ id: 'toolrun-8', kind: 'scheduled' });
		expect(wire.tool_args).toBe(toolArgs);
		expect(wire.tool_args).toEqual({ query: { tags: ['private', 'nested'], limit: 3 } });
	});
});
