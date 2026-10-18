import { describe, expect, it } from 'vitest';
import { mapToolRunCommandRow, mapToolRunEvent, mapToolRunPayload } from './toolRun.ts';

const toolRunId = 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85';
const sessionId = 'ses-0ecf371384ab79d597760ca41f2ba21c';
const toolName = 'files.write';

describe('tool run IPC contract', () => {
	it('maps a background ToolRun row with its required target tool identity', () => {
		expect(
			mapToolRunEvent({
				event: 'tool_run:finished',
				id: 4,
				payload: {
					tool_run_id: toolRunId,
					tool_name: toolName,
					kind: 'background',
					status: 'completed',
					session_id: sessionId,
					source_step_id: 'step-ea869e583dff7756d1747b32ad55e798',
					finished_at: '2026-08-26T00:00:00Z',
					future_wire_field: 'ignored',
				},
			}),
		).toEqual({
			event: 'tool_run:finished',
			id: 4,
			payload: {
				toolRunId,
				toolName,
				kind: 'background',
				status: 'completed',
				sessionId,
				sourceStepId: 'step-ea869e583dff7756d1747b32ad55e798',
				finishedAt: '2026-08-26T00:00:00Z',
			},
		});
	});

	it('requires the target tool name and only accepts background ToolRuns', () => {
		expect(mapToolRunPayload({ tool_run_id: toolRunId, kind: 'background' })).toBeNull();
		expect(
			mapToolRunPayload({ tool_run_id: toolRunId, kind: 'background', tool_name: '' }),
		).toBeNull();
		expect(
			mapToolRunPayload({ tool_run_id: toolRunId, kind: 'scheduled', tool_name: toolName }),
		).toBeNull();
		expect(
			mapToolRunPayload({ tool_run_id: toolRunId, kind: 'background', tool_name: toolName }),
		).toEqual({ toolRunId, toolName, kind: 'background' });
	});

	it('keeps omitted optional fields absent and rejects explicit nulls', () => {
		const row = { tool_run_id: toolRunId, tool_name: toolName, kind: 'background' };
		expect(mapToolRunPayload(row)).toEqual({ toolRunId, toolName, kind: 'background' });
		expect(mapToolRunPayload({ ...row, session_id: null })).toBeNull();
		expect(mapToolRunPayload({ ...row, status: null })).toBeNull();
		expect(mapToolRunPayload({ ...row, exit_code: 0 })).toEqual({
			toolRunId,
			toolName,
			kind: 'background',
		});
		expect(mapToolRunPayload({ ...row, source_step_id: 42 })).toBeNull();
	});

	it('requires a terminal or running status for history rows', () => {
		const row = { tool_run_id: toolRunId, tool_name: toolName, kind: 'background' };
		expect(mapToolRunPayload(row)).toEqual({ toolRunId, toolName, kind: 'background' });
		expect(mapToolRunCommandRow(row)).toBeNull();
		expect(mapToolRunCommandRow({ ...row, status: 'running' })).toEqual({
			toolRunId,
			toolName,
			kind: 'background',
			status: 'running',
		});
		expect(mapToolRunCommandRow({ ...row, status: 'waiting' })).toBeNull();
	});

	it('does not transform dynamic tool arguments into a board field', () => {
		const toolArgs = { query: { tags: ['private', 'nested'], limit: 3 } };
		const wire = {
			tool_run_id: toolRunId,
			tool_name: toolName,
			kind: 'background',
			tool_args: toolArgs,
		};
		expect(mapToolRunPayload(wire)).toEqual({ toolRunId, toolName, kind: 'background' });
		expect(wire.tool_args).toBe(toolArgs);
	});

	it('rejects unknown statuses and malformed canonical identities', () => {
		expect(mapToolRunPayload({
			tool_run_id: toolRunId,
			tool_name: toolName,
			kind: 'background',
			status: 'unexpected',
		})).toBeNull();
		expect(mapToolRunPayload({
			tool_run_id: 'toolrun-short',
			tool_name: toolName,
			kind: 'background',
		})).toBeNull();
		expect(mapToolRunPayload({
			tool_run_id: toolRunId,
			tool_name: toolName,
			kind: 'background',
			session_id: 'toolrun-58ec300e6432076c8ec176f56120ec01',
		})).toBeNull();
	});

	it('enforces each lifecycle channel shape and valid Tauri envelope IDs', () => {
		const event = (name: string, payload: object, id = 1) =>
			mapToolRunEvent({ event: name, id, payload });
		expect(event('tool_run:created', { tool_run_id: toolRunId, tool_name: toolName, kind: 'background' })).toBeNull();
		expect(event('tool_run:output', { tool_run_id: toolRunId, tool_name: toolName, kind: 'background', status: 'running', output: 'legacy' })).toBeNull();
		expect(event('tool_run:finished', { tool_run_id: toolRunId, tool_name: toolName, kind: 'background', status: 'running' })).toBeNull();
		expect(event('tool_run:updated', { tool_run_id: toolRunId, tool_name: toolName, kind: 'background', session_id: sessionId })).not.toBeNull();
		expect(event('tool_run:finished', { tool_run_id: toolRunId, tool_name: toolName, kind: 'background', status: 'completed' }, Number.NaN)).toBeNull();
	});
});
