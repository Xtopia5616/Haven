import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cancelToolRunCommand, listToolRunHistory, listToolRunRows } from './toolRunCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('ToolRun command contract boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('normalizes list_tool_runs rows through the shared ToolRunEvent mapper', async () => {
		invokeMock.mockResolvedValue([
			{
				tool_run_id: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
				kind: 'background',
				status: 'running',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				future_wire_field: 'ignored',
			},
			{ tool_run_id: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1', kind: 'future-kind' },
			{ tool_run_id: 'toolrun-f7391f4e56282cae467ea5a88dc7a560', kind: 'scheduled' },
		] as never);

		await expect(listToolRunRows()).resolves.toEqual([
			{
				toolRunId: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
				kind: 'background',
				status: 'running',
				sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			},
			null,
			null,
		]);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_tool_runs');
	});

	it('keeps malformed top-level list responses as a no-op', async () => {
		invokeMock.mockResolvedValue({ rows: [] } as never);

		await expect(listToolRunRows()).resolves.toBeNull();
	});

	it('binds filtered ToolRun history rows to the requested session', async () => {
		const sessionId = 'ses-0000000000000000000000000000001f';
		invokeMock.mockResolvedValueOnce([
			{
				tool_run_id: 'toolrun-00000000000000000000000000000020',
				kind: 'background',
				status: 'completed',
				session_id: sessionId,
			},
			{
				tool_run_id: 'toolrun-00000000000000000000000000000021',
				kind: 'scheduled',
				status: 'completed',
				session_id: 'ses-00000000000000000000000000000022',
			},
			{
				tool_run_id: 'toolrun-00000000000000000000000000000023',
				kind: 'scheduled',
				status: 'completed',
			},
		] as never);

		await expect(listToolRunHistory(undefined, 50, sessionId)).resolves.toEqual([
			{
				toolRunId: 'toolrun-00000000000000000000000000000020',
				kind: 'background',
				status: 'completed',
				sessionId,
			},
		]);
		expect(invokeMock).toHaveBeenCalledWith('list_tool_run_history', {
			kind: null,
			limit: 50,
			sessionId,
		});
	});

	it('uses the named cancel request and preserves the boolean response', async () => {
		invokeMock.mockResolvedValue(true);
		const request = {
			toolRunId: 'toolrun-f7391f4e56282cae467ea5a88dc7a560',
			kind: 'scheduled' as const,
		};

		await expect(cancelToolRunCommand(request)).resolves.toBe(true);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('cancel_tool_run', request);
	});

	it('rejects a malformed cancellation boolean response', async () => {
		invokeMock.mockResolvedValue('cancelled' as never);

		await expect(
			cancelToolRunCommand({
				toolRunId: 'toolrun-8c538cfe87353df186d2165b761fb38b',
				kind: 'background',
			}),
		).rejects.toThrow('Invalid cancel_tool_run boolean response');
	});
});
