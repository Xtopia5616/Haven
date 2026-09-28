import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cancelActionCommand, listActionRows } from './actionCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('Action command contract boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('normalizes list_actions rows through the shared ActionEvent mapper', async () => {
		invokeMock.mockResolvedValue([
			{
				id: 'act-1',
				kind: 'background',
				status: 'running',
				session_id: 'ses-1',
				future_wire_field: 'ignored',
			},
			{ id: 'act-2', kind: 'future-kind' },
		] as never);

		await expect(listActionRows()).resolves.toEqual([
			{ id: 'act-1', kind: 'background', status: 'running', sessionId: 'ses-1' },
			null,
		]);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_actions');
	});

	it('keeps malformed top-level list responses as a no-op', async () => {
		invokeMock.mockResolvedValue({ rows: [] } as never);

		await expect(listActionRows()).resolves.toBeNull();
	});

	it('uses the named cancel request and preserves the boolean response', async () => {
		invokeMock.mockResolvedValue(true);
		const request = { actionId: 'act-3', kind: 'scheduled' as const };

		await expect(cancelActionCommand(request)).resolves.toBe(true);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('cancel_action', request);
	});

	it('returns the cancellation invoke promise unchanged', () => {
		const result = Promise.resolve(true);
		invokeMock.mockReturnValue(result);

		expect(cancelActionCommand({ actionId: 'act-4', kind: 'background' })).toBe(result);
	});
});
