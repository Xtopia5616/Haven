import { describe, expect, it } from 'vitest';
import { mapToolRunCommandRow, mapToolRunEvent, mapToolRunPayload } from './toolRun.ts';

describe('tool run IPC contract', () => {
	it('maps a normal Rust ToolRunEvent to the UI DTO', () => {
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: {
					tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
					kind: 'background',
					status: 'completed',
					session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
					source_step_id: 'step-6b86b273ff34fce19d6b804eff5a3f57',
					finished_at: '2026-08-26T00:00:00Z',
					exit_code: 0,
					future_wire_field: 'ignored',
				},
			}),
		).toEqual({
			event: 'tool_run:finished',
			id: 4,
			payload: {
				toolRunId: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
				kind: 'background',
				status: 'completed',
				sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				sourceStepId: 'step-6b86b273ff34fce19d6b804eff5a3f57',
				finishedAt: '2026-08-26T00:00:00Z',
				exitCode: 0,
			},
		});
	});

	it('keeps omitted optional fields absent and rejects explicit nulls', () => {
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
				kind: 'scheduled',
			}),
		).toEqual({
			toolRunId: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
			kind: 'scheduled',
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
				kind: 'scheduled',
				session_id: null,
				status: null,
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
				kind: 'scheduled',
				exit_code: null,
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
				kind: 'background',
				source_step_id: 42,
			}),
		).toBeNull();
	});

	it('requires status for command rows while keeping the shared payload projection sparse', () => {
		const row = {
			tool_run_id: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
			kind: 'scheduled',
		};

		expect(mapToolRunPayload(row)).toEqual({ toolRunId: row.tool_run_id, kind: 'scheduled' });
		expect(mapToolRunCommandRow(row)).toBeNull();
		expect(mapToolRunCommandRow({ ...row, status: 'waiting' })).toEqual({
			toolRunId: row.tool_run_id,
			kind: 'scheduled',
			status: 'waiting',
		});
		expect(mapToolRunCommandRow({ ...row, status: 'future-status' })).toBeNull();
	});

	it('uses generated ScheduleMode values for ToolRun lifecycle rows', () => {
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-19581e27de7ced00ff1ce50b2047e7a5',
				kind: 'scheduled',
				mode: 'continue',
			}),
		).toEqual({
			toolRunId: 'toolrun-19581e27de7ced00ff1ce50b2047e7a5',
			kind: 'scheduled',
			mode: 'continue',
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-4a44dc15364204a80fe80e9039455cc1',
				kind: 'scheduled',
				mode: 'future_mode',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-4fc82b26aecb47d2868c4efbe3581732',
				kind: 'scheduled',
				mode: null,
			}),
		).toBeNull();
	});

	it('rejects unknown ToolRun statuses and kinds', () => {
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-4e07408562bedb8b60ce05c1decfe3ad',
				kind: 'background',
				status: 'unexpected',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-4b227777d4dd1fc61c6f884f48641d02',
				kind: 'future-kind',
				status: 'running',
			}),
		).toBeNull();
	});

	it('fails closed when required fields are missing or malformed', () => {
		expect(mapToolRunPayload({ kind: 'background' })).toBeNull();
		expect(
			mapToolRunPayload({
				id: 'toolrun-c49fea7425fa7f8699897a97c159c669',
				kind: 'background',
			}),
		).toBeNull();
		expect(mapToolRunPayload({ tool_run_id: '', kind: 'background' })).toBeNull();
		expect(
			mapToolRunPayload({ tool_run_id: 'toolrun-ef2d127de37b942baad06145e54b0c61' }),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-e7f6c011776e8db7cd330b54174fd76f',
				kind: 'background',
				session_id: 42,
			}),
		).toBeNull();
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: { tool_run_id: 'toolrun-7902699be42c8a8e46fbbb4501726517' },
			}),
		).toBeNull();
	});

	it('rejects invalid Tauri envelope ids consistently', () => {
		for (const id of [Number.NaN, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
			expect(
				mapToolRunEvent({
					event: 'tool_run:finished',
					id,
					payload: { tool_run_id: 'toolrun-7902699be42c8a8e46fbbb4501726517' },
				}),
			).toBeNull();
		}
	});

	it('rejects non-canonical ToolRun, session, and source-step identities', () => {
		const validToolRunId = 'toolrun-0123456789abcdef0123456789abcdef';
		for (const tool_run_id of [
			'toolrun-short',
			'ses-0123456789abcdef0123456789abcdef',
			'toolrun-01234567-89ab-cdef-0123-456789abcdef',
			'toolrun-0123456789ABCDEF0123456789abcdef',
		]) {
			expect(mapToolRunPayload({ tool_run_id, kind: 'scheduled' })).toBeNull();
		}
		expect(
			mapToolRunPayload({
				tool_run_id: validToolRunId,
				kind: 'background',
				session_id: 'toolrun-0123456789abcdef0123456789abcdef',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: validToolRunId,
				kind: 'background',
				session_id: 'ses-0123456789abcdef0123456789abcdef',
				source_step_id: 'msg-0123456789abcdef0123456789abcdef',
			}),
		).toBeNull();
	});

	it('does not transform dynamic tool_args into a board field', () => {
		const toolArgs = { query: { tags: ['private', 'nested'], limit: 3 } };
		const wire = {
			tool_run_id: 'toolrun-2c624232cdd221771294dfbb310aca00',
			kind: 'scheduled',
			tool_args: toolArgs,
		};

		// ToolRunEvent intentionally excludes execution arguments. Keep the JSON
		// extension opaque to this UI DTO mapper rather than stringifying it.
		expect(mapToolRunPayload(wire)).toEqual({
			toolRunId: 'toolrun-2c624232cdd221771294dfbb310aca00',
			kind: 'scheduled',
		});
		expect(wire.tool_args).toBe(toolArgs);
		expect(wire.tool_args).toEqual({ query: { tags: ['private', 'nested'], limit: 3 } });
	});
});
