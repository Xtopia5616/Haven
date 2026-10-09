import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	deleteAllSessions,
	deleteSession,
	continueSession,
	endSession,
	getLatestSessionForResume,
	getSessionLineage,
	getSessionForResume,
	interruptSession,
	listRuntimeSessions,
	listSessionHistory,
	rollbackSession,
	reopenSession,
	searchSessionHistoryFiltered,
	updateSessionTitle,
} from './sessionCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('session history command boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('keeps active-session list rows unchanged', async () => {
		const response = {
			sessions: [{ id: 'ses-1', waiting_reason: 'interaction', future_field: true }],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(listRuntimeSessions()).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_runtime_sessions');
	});

	it('loads the lineage for the selected session with a flat request', async () => {
		const request = { sessionId: 'ses-child' };
		const response = { parent: { id: 'ses-parent' }, children: [] };
		invokeMock.mockResolvedValue(response as never);

		await expect(getSessionLineage(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_session_lineage', request);
	});

	it('lists a persisted history page through the list command', async () => {
		const request = { limit: 50, offset: 0 };
		const response = [{ id: 'ses-2', input_text: 'hello' }];
		invokeMock.mockResolvedValue(response as never);

		await expect(listSessionHistory(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_session_history', request);
	});

	it('passes the history filter flat and preserves response fields', async () => {
		const request = {
			query: 'hello',
			status: null,
			startDate: '2026-01-01',
			endDate: null,
			limit: 50,
			offset: 0,
		};
		const response = [{ id: 'ses-2', input_text: 'hello', future_field: 'kept' }];
		invokeMock.mockResolvedValue(response as never);

		await expect(searchSessionHistoryFiltered(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('search_session_history_filtered', request);
	});

	it('forwards resume reads and startup restore without response mapping', async () => {
		const request = { sessionId: 'ses-3' };
		const resume = { session: { id: 'ses-3' }, future_field: { retained: true } };
		invokeMock.mockResolvedValueOnce(resume as never).mockResolvedValueOnce(null);

		await expect(getSessionForResume(request)).resolves.toBe(resume);
		await expect(getLatestSessionForResume()).resolves.toBeNull();
		expect(invokeMock.mock.calls).toEqual([
			['get_session_for_resume', request],
			['get_latest_session_for_resume'],
		]);
	});

	it('owns rollback and active session lifecycle commands', async () => {
		const request = { sessionId: 'ses-lifecycle' };
		const rollbackRequest = {
			sessionId: request.sessionId,
			targetStep: 3,
			pause: true,
			targetMessageId: 'msg-00000000000000000000000000000001',
		};
		invokeMock.mockResolvedValue(undefined as never);

		await rollbackSession(rollbackRequest);
		await endSession(request);
		await interruptSession(request);
		await continueSession(request);

		expect(invokeMock.mock.calls).toEqual([
			['rollback_session', rollbackRequest],
			['end_session', request],
			['interrupt_session', request],
			['continue_session', request],
		]);
	});

	it('forwards history mutations and keeps the command result values', async () => {
		const request = { sessionId: 'ses-4' };
		invokeMock.mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined);
		invokeMock.mockResolvedValueOnce(2).mockResolvedValueOnce(undefined);

		await expect(reopenSession(request)).resolves.toBeUndefined();
		await expect(deleteSession(request)).resolves.toBeUndefined();
		await expect(deleteAllSessions()).resolves.toBe(2);
		await expect(updateSessionTitle({ ...request, title: 'Updated' })).resolves.toBeUndefined();
		expect(invokeMock.mock.calls).toEqual([
			['reopen_session', request],
			['delete_session', request],
			['delete_all_sessions'],
			['update_session_title', { ...request, title: 'Updated' }],
		]);
	});

	it('returns the underlying invoke promise unchanged', () => {
		const pending = Promise.resolve({ session: { id: 'ses-5' } });
		invokeMock.mockReturnValue(pending as never);

		expect(getSessionForResume({ sessionId: 'ses-5' })).toBe(pending);
	});
});
