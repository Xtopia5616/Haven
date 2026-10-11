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
			sessions: [
				{
					id: 'ses-00000000000000000000000000000001',
					steps: [],
					waiting_reason: 'interaction',
					future_field: true,
				},
			],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(listRuntimeSessions()).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_runtime_sessions');
	});

	it('loads the lineage for the selected session with a flat request', async () => {
		const request = { sessionId: 'ses-child' };
		const response = {
			parent: { id: 'ses-00000000000000000000000000000002' },
			children: [],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(getSessionLineage(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_session_lineage', request);
	});

	it('lists a persisted history page through the list command', async () => {
		const request = { limit: 50, offset: 0 };
		const response = [{ id: 'ses-00000000000000000000000000000003', input_text: 'hello' }];
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
		const response = [
			{
				id: 'ses-00000000000000000000000000000003',
				input_text: 'hello',
				future_field: 'kept',
			},
		];
		invokeMock.mockResolvedValue(response as never);

		await expect(searchSessionHistoryFiltered(request)).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('search_session_history_filtered', request);
	});

	it('validates resume identities and accepts an empty latest-session result', async () => {
		const request = { sessionId: 'ses-3' };
		const resume = {
			session: { id: 'ses-00000000000000000000000000000004' },
			messages: [],
			steps: [],
			llm_usage: [],
			interactions: [],
			future_field: { retained: true },
		};
		invokeMock.mockResolvedValueOnce(resume as never).mockResolvedValueOnce(null);

		await expect(getSessionForResume(request)).resolves.toBe(resume);
		await expect(getLatestSessionForResume()).resolves.toBeNull();
		expect(invokeMock.mock.calls).toEqual([
			['get_session_for_resume', request],
			['get_latest_session_for_resume'],
		]);
	});

	it('rejects command responses with non-canonical session identities', async () => {
		invokeMock.mockResolvedValueOnce({ sessions: [{ id: 'ses-short', steps: [] }] } as never);
		await expect(listRuntimeSessions()).rejects.toThrow('Invalid runtime session identity');

		invokeMock.mockResolvedValueOnce({
			session: { id: 'ses-00000000000000000000000000000005' },
			messages: [
				{
					id: 'msg-00000000000000000000000000000006',
					session_id: 'ses-short',
				},
			],
			steps: [],
			llm_usage: [],
			interactions: [],
		} as never);
		await expect(getLatestSessionForResume()).rejects.toThrow(
			'Invalid resumed message identity',
		);
	});

	it('requires step-backed transcript messages to share a canonical step identity', async () => {
		const sessionId = 'ses-00000000000000000000000000000008';
		const stepId = 'step-00000000000000000000000000000009';
		const response = {
			session: { id: sessionId },
			messages: [{ id: stepId, session_id: sessionId }],
			steps: [{ id: stepId, session_id: sessionId }],
			llm_usage: [],
			interactions: [],
		};
		invokeMock.mockResolvedValueOnce(response as never);
		await expect(getSessionForResume({ sessionId })).resolves.toBe(response);

		invokeMock.mockResolvedValueOnce({ ...response, steps: [] } as never);
		await expect(getSessionForResume({ sessionId })).rejects.toThrow(
			'Invalid resumed message identity',
		);
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

	it('resolves the generated command result after identity validation', async () => {
		const response = {
			session: { id: 'ses-00000000000000000000000000000007' },
			messages: [],
			steps: [],
			llm_usage: [],
			interactions: [],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(getSessionForResume({ sessionId: 'ses-5' })).resolves.toBe(response);
	});
});
