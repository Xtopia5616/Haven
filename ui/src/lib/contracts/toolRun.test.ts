import { describe, expect, it } from 'vitest';
import { mapToolRunCommandRow, mapToolRunEvent, mapToolRunPayload } from './toolRun.ts';

describe('tool run IPC contract', () => {
	it('maps a normal Rust ToolRunEvent to the UI DTO', () => {
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: {
					tool_run_id: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
					kind: 'background',
					status: 'completed',
					session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
					source_step_id: 'step-ea869e583dff7756d1747b32ad55e798',
					finished_at: '2026-08-26T00:00:00Z',
					exit_code: 0,
					future_wire_field: 'ignored',
				},
			}),
		).toEqual({
			event: 'tool_run:finished',
			id: 4,
			payload: {
				toolRunId: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
				kind: 'background',
				status: 'completed',
				sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				sourceStepId: 'step-ea869e583dff7756d1747b32ad55e798',
				finishedAt: '2026-08-26T00:00:00Z',
				exitCode: 0,
			},
		});
	});

	it('keeps omitted optional fields absent and rejects explicit nulls', () => {
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
				kind: 'scheduled',
			}),
		).toEqual({
			toolRunId: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
			kind: 'scheduled',
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
				kind: 'scheduled',
				session_id: null,
				status: null,
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
				kind: 'scheduled',
				exit_code: null,
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
				kind: 'background',
				source_step_id: 42,
			}),
		).toBeNull();
	});

	it('requires status for command rows while keeping the shared payload projection sparse', () => {
		const row = {
			tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
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
				tool_run_id: 'toolrun-253cea9a73bd446583b8d67ee7881e04',
				kind: 'scheduled',
				mode: 'continue',
			}),
		).toEqual({
			toolRunId: 'toolrun-253cea9a73bd446583b8d67ee7881e04',
			kind: 'scheduled',
			mode: 'continue',
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-a1a4416f8b4eb96a19daa467cf021c40',
				kind: 'scheduled',
				mode: 'future_mode',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-85636ca1f3d6a5740d35f1ddcf3409ab',
				kind: 'scheduled',
				mode: null,
			}),
		).toBeNull();
	});

	it('rejects unknown ToolRun statuses and kinds', () => {
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-f7391f4e56282cae467ea5a88dc7a560',
				kind: 'background',
				status: 'unexpected',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-8c538cfe87353df186d2165b761fb38b',
				kind: 'future-kind',
				status: 'running',
			}),
		).toBeNull();
	});

	it('maps bounded Workflow progress and safe retry availability', () => {
		const projected = mapToolRunPayload({
			tool_run_id: 'toolrun-00c73d0e8748c1a18702bf89c4bd4353',
			kind: 'scheduled',
			status: 'failed',
			workflow_retry_available: true,
			error_reason: 'child authorization rejected',
		});
		expect(projected).toMatchObject({
			workflowRetryAvailable: true,
			errorReason: 'child authorization rejected',
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-00c73d0e8748c1a18702bf89c4bd4353',
				kind: 'scheduled',
				status: 'running',
				workflow_progress: { step_id: 'read_source', step_index: 2, step_total: 4 },
			}),
		).toMatchObject({
			workflowProgress: { stepId: 'read_source', stepIndex: 2, stepTotal: 4 },
		});
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-00c73d0e8748c1a18702bf89c4bd4353',
				kind: 'background',
				status: 'failed',
				workflow_retry_available: true,
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-00c73d0e8748c1a18702bf89c4bd4353',
				kind: 'scheduled',
				status: 'running',
				workflow_progress: { step_id: 'secret/name', step_index: 1, step_total: 1 },
			}),
		).toBeNull();
	});

	it('enforces the required fields of each lifecycle channel', () => {
		const toolRunId = 'toolrun-58ec300e6432076c8ec176f56120ec01';
		const event = (
			name: 'tool_run:created' | 'tool_run:updated' | 'tool_run:output' | 'tool_run:finished',
			payload: object,
		) =>
			mapToolRunEvent({ event: name, id: 1, payload });

		expect(event('tool_run:created', { tool_run_id: toolRunId, kind: 'background' })).toBeNull();
		expect(
			event('tool_run:output', {
				tool_run_id: toolRunId,
				kind: 'background',
				status: 'running',
			}),
		).toBeNull();
		expect(
			event('tool_run:output', {
				tool_run_id: toolRunId,
				kind: 'background',
				status: 'completed',
				output: 'late output',
			}),
		).toBeNull();
		expect(
			event('tool_run:finished', {
				tool_run_id: toolRunId,
				kind: 'background',
				status: 'running',
			}),
		).toBeNull();
		expect(
			event('tool_run:updated', {
				tool_run_id: toolRunId,
				kind: 'background',
				session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
			}),
		).not.toBeNull();
	});

	it('fails closed when required fields are missing or malformed', () => {
		expect(mapToolRunPayload({ kind: 'background' })).toBeNull();
		expect(
			mapToolRunPayload({
				id: 'toolrun-7dac828ec7db92e6714d63fcad6d882a',
				kind: 'background',
			}),
		).toBeNull();
		expect(mapToolRunPayload({ tool_run_id: '', kind: 'background' })).toBeNull();
		expect(
			mapToolRunPayload({ tool_run_id: 'toolrun-66ab9a878b9ddd58f81ed42d68529c44' }),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: 'toolrun-3719ed23050d15defc73068cde1b1dc0',
				kind: 'background',
				session_id: 42,
			}),
		).toBeNull();
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: { tool_run_id: 'toolrun-78c38c07f72fe515ebef1de39c4b837b' },
			}),
		).toBeNull();
	});

	it('rejects invalid Tauri envelope ids consistently', () => {
		for (const id of [Number.NaN, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
			expect(
				mapToolRunEvent({
					event: 'tool_run:finished',
					id,
					payload: { tool_run_id: 'toolrun-78c38c07f72fe515ebef1de39c4b837b' },
				}),
			).toBeNull();
		}
	});

	it('rejects non-canonical ToolRun, session, and source-step identities', () => {
		const validToolRunId = 'toolrun-58ec300e6432076c8ec176f56120ec01';
		for (const tool_run_id of [
			'toolrun-short',
			'ses-103ac63490bc5f6c27fd45405efecd69',
			'toolrun-1',
			'toolrun-0123456789abcdef0123456789abcdeF',
		]) {
			expect(mapToolRunPayload({ tool_run_id, kind: 'scheduled' })).toBeNull();
		}
		expect(
			mapToolRunPayload({
				tool_run_id: validToolRunId,
				kind: 'background',
				session_id: 'toolrun-58ec300e6432076c8ec176f56120ec01',
			}),
		).toBeNull();
		expect(
			mapToolRunPayload({
				tool_run_id: validToolRunId,
				kind: 'background',
				session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
				source_step_id: 'msg-fc16f96c92a9c37a323bb7d0f47a4e22',
			}),
		).toBeNull();
	});

	it('does not transform dynamic tool_args into a board field', () => {
		const toolArgs = { query: { tags: ['private', 'nested'], limit: 3 } };
		const wire = {
			tool_run_id: 'toolrun-772f25b617df7ead1e3854501b05d800',
			kind: 'scheduled',
			tool_args: toolArgs,
		};

		// ToolRunEvent intentionally excludes execution arguments. Keep the JSON
		// extension opaque to this UI DTO mapper rather than stringifying it.
		expect(mapToolRunPayload(wire)).toEqual({
			toolRunId: 'toolrun-772f25b617df7ead1e3854501b05d800',
			kind: 'scheduled',
		});
		expect(wire.tool_args).toBe(toolArgs);
		expect(wire.tool_args).toEqual({ query: { tags: ['private', 'nested'], limit: 3 } });
	});
});
