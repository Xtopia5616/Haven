import { beforeEach, describe, expect, it, vi } from 'vitest';
import { clearFacts } from './memoryCommands.ts';
import {
	countSessionHistory,
	countSessionHistorySearch,
	deleteAllSessions,
} from './sessionCommands.ts';
import { resetSessionPermissions, runMemoryMaintenance } from './settingsCommands.ts';
import { clearToolRunHistory } from './toolRunCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('count command boundaries', () => {
	beforeEach(() => invokeMock.mockReset());

	it('validates every Rust integer count response before returning it', async () => {
		const commands = [
			() => clearFacts(),
			() => clearToolRunHistory(),
			() => countSessionHistory(),
			() => countSessionHistorySearch({ query: 'hello' }),
			() => deleteAllSessions(),
			() => resetSessionPermissions(),
			() => runMemoryMaintenance(),
		];

		for (const command of commands) {
			invokeMock.mockResolvedValueOnce(7 as never);
			await expect(command()).resolves.toBe(7);

			invokeMock.mockResolvedValueOnce((Number.MAX_SAFE_INTEGER + 1) as never);
			await expect(command()).rejects.toThrow(/count response/);
		}

		expect(invokeMock.mock.calls).toEqual([
			['clear_facts'],
			['clear_facts'],
			['clear_tool_run_history'],
			['clear_tool_run_history'],
			['count_session_history'],
			['count_session_history'],
			['count_session_history_search', { query: 'hello' }],
			['count_session_history_search', { query: 'hello' }],
			['delete_all_sessions'],
			['delete_all_sessions'],
			['reset_session_permissions'],
			['reset_session_permissions'],
			['run_memory_maintenance'],
			['run_memory_maintenance'],
		]);
	});
});
