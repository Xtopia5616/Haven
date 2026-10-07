import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cancelToolRunCommand, listToolRunRows } from './toolRunCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('ToolRun command contract boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('normalizes list_tool_runs rows through the shared ToolRunEvent mapper', async () => {
		invokeMock.mockResolvedValue([
			{
				tool_run_id: 'toolrun-1',
				kind: 'background',
				status: 'running',
				session_id: 'ses-1',
				future_wire_field: 'ignored',
			},
			{ tool_run_id: 'toolrun-2', kind: 'future-kind' },
		] as never);

		await expect(listToolRunRows()).resolves.toEqual([
			{ toolRunId: 'toolrun-1', kind: 'background', status: 'running', sessionId: 'ses-1' },
			null,
		]);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_tool_runs');
	});

	it('keeps malformed top-level list responses as a no-op', async () => {
		invokeMock.mockResolvedValue({ rows: [] } as never);

		await expect(listToolRunRows()).resolves.toBeNull();
	});

	it('uses the named cancel request and preserves the boolean response', async () => {
		invokeMock.mockResolvedValue(true);
		const request = { toolRunId: 'toolrun-3', kind: 'scheduled' as const };

		await expect(cancelToolRunCommand(request)).resolves.toBe(true);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('cancel_tool_run', request);
	});

	it('returns the cancellation invoke promise unchanged', () => {
		const result = Promise.resolve(true);
		invokeMock.mockReturnValue(result);

		expect(cancelToolRunCommand({ toolRunId: 'toolrun-4', kind: 'background' })).toBe(result);
	});
});
