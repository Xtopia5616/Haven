import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	clearHistory,
	deleteSession,
	getLastConversation,
	getSessionLineage,
	getSessionForResume,
	listSessions,
	listHistory,
	reopenSession,
	searchHistoryFiltered,
	updateSessionTitle,
} from './sessionHistoryCommands.ts';

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

		await expect(listSessions()).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_sessions');
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

		await expect(listHistory(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_history', request);
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

		await expect(searchHistoryFiltered(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('search_history_filtered', request);
	});

	it('forwards resume reads and startup restore without response mapping', async () => {
		const request = { sessionId: 'ses-3' };
		const resume = { session: { id: 'ses-3' }, future_field: { retained: true } };
		invokeMock.mockResolvedValueOnce(resume as never).mockResolvedValueOnce(null);

		await expect(getSessionForResume(request)).resolves.toBe(resume);
		await expect(getLastConversation()).resolves.toBeNull();
		expect(invokeMock.mock.calls).toEqual([
			['get_session_for_resume', request],
			['get_last_conversation'],
		]);
	});

	it('uses an injected typed invoker for controller tests and preserves the request', async () => {
		const request = { sessionId: 'ses-injected' };
		const response = { session: { id: request.sessionId } };
		const invokeCommand = vi.fn().mockResolvedValue(response);

		await expect(getSessionForResume(request, invokeCommand)).resolves.toBe(response);
		expect(invokeCommand).toHaveBeenCalledOnce();
		expect(invokeCommand).toHaveBeenCalledWith('get_session_for_resume', request);
	});

	it('forwards history mutations and keeps the command result values', async () => {
		const request = { sessionId: 'ses-4' };
		invokeMock.mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined);
		invokeMock.mockResolvedValueOnce(2).mockResolvedValueOnce(undefined);

		await expect(reopenSession(request)).resolves.toBeUndefined();
		await expect(deleteSession(request)).resolves.toBeUndefined();
		await expect(clearHistory()).resolves.toBe(2);
		await expect(updateSessionTitle({ ...request, title: 'Updated' })).resolves.toBeUndefined();
		expect(invokeMock.mock.calls).toEqual([
			['reopen_session', request],
			['delete_session', request],
			['clear_history'],
			['update_session_title', { ...request, title: 'Updated' }],
		]);
	});

	it('returns the underlying invoke promise unchanged', () => {
		const pending = Promise.resolve({ session: { id: 'ses-5' } });
		invokeMock.mockReturnValue(pending as never);

		expect(getSessionForResume({ sessionId: 'ses-5' })).toBe(pending);
	});
});
