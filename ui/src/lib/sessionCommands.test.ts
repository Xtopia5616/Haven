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
	processTranscript,
	rollbackSession,
	reopenSession,
	searchSessionHistoryFiltered,
	updateSessionTitle,
} from './sessionCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

const sessionId = 'ses-00000000000000000000000000000001';

function sessionRecord(id = sessionId) {
	return {
		session_id: id,
		input_text: 'hello',
		title: null,
		status: 'completed',
		created_at: '2026-10-12T08:00:00Z',
		updated_at: '2026-10-12T08:01:00Z',
		creation: { creator: 'user', method: 'new', source: { kind: 'none' } },
	};
}

function runtimeSession(id = sessionId) {
	return {
		session_id: id,
		input_text: 'hello',
		title: null,
		status: 'paused',
		waiting_reason: 'ask',
	};
}

function resumeResponse(id = sessionId) {
	return {
		session: sessionRecord(id),
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
	};
}

function resumeMessage(messageId: string, ownerSessionId: string) {
	return {
		message_id: messageId,
		session_id: ownerSessionId,
		role: 'assistant',
		content: 'Working',
		message_type: 'thought',
		created_at: '2026-10-12T08:00:01Z',
		tool_call_id: null,
		attachments: [],
		voice: false,
	};
}

function resumeStep(stepId: string, ownerSessionId: string) {
	return {
		step_id: stepId,
		session_id: ownerSessionId,
		step_number: 1,
		tool_index: 0,
		thought: null,
		tool_name: 'read_file',
		tool_input: '{}',
		tool_call_id: null,
		observation: null,
		status: 'completed',
		is_high_risk: false,
		confirmed: null,
		silent: false,
		started_at: null,
		completed_at: null,
		created_at: '2026-10-12T08:00:01Z',
	};
}

function rendererSessionRecord(id = sessionId) {
	return {
		sessionId: id,
		input_text: 'hello',
		title: null,
		status: 'completed',
		created_at: '2026-10-12T08:00:00Z',
		updated_at: '2026-10-12T08:01:00Z',
		creation: { creator: 'user', method: 'new', source: { kind: 'none' } },
	};
}

function rendererResumeResponse(id = sessionId) {
	return {
		session: rendererSessionRecord(id),
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
	};
}

describe('session history command boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('submits transcripts through the Session command adapter and validates identity', async () => {
		const request = {
			transcript: 'hello',
			activeSessionId: null,
			attachments: null,
			voice: false,
		};
		const response = {
			SessionCreated: {
				session_id: 'ses-0000000000000000000000000000000a',
				message_id: 'msg-0000000000000000000000000000000b',
			},
		};
		invokeMock.mockResolvedValueOnce(response as never);

		await expect(processTranscript(request)).resolves.toEqual({
			kind: 'session_created',
			sessionId: 'ses-0000000000000000000000000000000a',
			messageId: 'msg-0000000000000000000000000000000b',
		});
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('process_transcript', request);

		invokeMock.mockResolvedValueOnce({
			SessionCreated: { session_id: 'ses-short' },
		} as never);
		await expect(processTranscript(request)).rejects.toThrow(
			'Invalid process transcript identity',
		);

		const supplementRequest = {
			...request,
			activeSessionId: 'ses-0000000000000000000000000000000c',
		};
		const supplementResponse = {
			Supplemented: {
				session_id: supplementRequest.activeSessionId,
				message_id: 'msg-0000000000000000000000000000000d',
			},
		};
		invokeMock.mockResolvedValueOnce(supplementResponse as never);
		await expect(processTranscript(supplementRequest)).resolves.toEqual({
			kind: 'supplemented',
			sessionId: supplementRequest.activeSessionId,
			messageId: 'msg-0000000000000000000000000000000d',
		});

		invokeMock.mockResolvedValueOnce({
			Supplemented: {
				session_id: 'ses-0000000000000000000000000000000e',
				message_id: 'msg-0000000000000000000000000000000f',
			},
		} as never);
		await expect(processTranscript(supplementRequest)).rejects.toThrow(
			'Invalid process transcript identity',
		);
	});

	it('maps the runtime session allowlist and drops unknown fields', async () => {
		const response = {
			sessions: [{ ...runtimeSession(), future_field: true }],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(listRuntimeSessions()).resolves.toEqual({
			sessions: [
				{
					sessionId,
					inputText: 'hello',
					title: null,
					status: 'paused',
					waitingReason: 'ask',
				},
			],
		});
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_runtime_sessions');
	});

	it('loads the lineage for the selected session with a flat request', async () => {
		const request = { sessionId: 'ses-00000000000000000000000000000001' };
		const response = {
			session_id: request.sessionId,
			parent_session_id: 'ses-00000000000000000000000000000002',
			parent: sessionRecord('ses-00000000000000000000000000000002'),
			children: [],
		};
		invokeMock.mockResolvedValue(response as never);

		await expect(getSessionLineage(request)).resolves.toEqual({
			sessionId: request.sessionId,
			parentSessionId: 'ses-00000000000000000000000000000002',
			parent: rendererSessionRecord('ses-00000000000000000000000000000002'),
			children: [],
		});
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_session_lineage', request);
	});

	it('rejects lineage rows whose owner does not match the request', async () => {
		const request = { sessionId: 'ses-00000000000000000000000000000015' };
		invokeMock.mockResolvedValueOnce({
			session_id: 'ses-00000000000000000000000000000016',
			parent_session_id: null,
			parent: null,
			children: [],
		} as never);
		await expect(getSessionLineage(request)).rejects.toThrow(
			'Invalid session lineage identity',
		);
	});

	it('lists a persisted history page through the list command', async () => {
		const request = { limit: 50, offset: 0 };
		const response = [sessionRecord('ses-00000000000000000000000000000003')];
		invokeMock.mockResolvedValue(response as never);

		await expect(listSessionHistory(request)).resolves.toEqual([
			rendererSessionRecord('ses-00000000000000000000000000000003'),
		]);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('list_session_history', request);
	});

	it('passes the history filter flat and maps response fields', async () => {
		const request = {
			query: 'hello',
			status: null,
			startDate: '2026-01-01',
			endDate: null,
			limit: 50,
			offset: 0,
		};
		const response = [
			{ ...sessionRecord('ses-00000000000000000000000000000003'), future_field: 'kept' },
		];
		invokeMock.mockResolvedValue(response as never);

		await expect(searchSessionHistoryFiltered(request)).resolves.toEqual([
			{
				...rendererSessionRecord('ses-00000000000000000000000000000003'),
				future_field: 'kept',
			},
		]);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('search_session_history_filtered', request);
	});

	it('binds resume identity to the request and accepts an empty latest-session result', async () => {
		const request = { sessionId: 'ses-00000000000000000000000000000003' };
		const resume = { ...resumeResponse(request.sessionId), future_field: { retained: true } };
		invokeMock.mockResolvedValueOnce(resume as never).mockResolvedValueOnce(null);

		await expect(getSessionForResume(request)).resolves.toEqual(
			{
				...rendererResumeResponse(request.sessionId),
				future_field: { retained: true },
			},
		);
		await expect(getLatestSessionForResume()).resolves.toBeNull();
		expect(invokeMock.mock.calls).toEqual([
			['get_session_for_resume', request],
			['get_latest_session_for_resume'],
		]);
	});

	it('rejects a canonical resume response owned by another requested session', async () => {
		invokeMock.mockResolvedValueOnce(
			resumeResponse('ses-00000000000000000000000000000018') as never,
		);
		await expect(
			getSessionForResume({ sessionId: 'ses-00000000000000000000000000000019' }),
		).rejects.toThrow('Invalid resumed session identity');
	});

	it('rejects command responses with non-canonical session identities', async () => {
		invokeMock.mockResolvedValueOnce({
			sessions: [{ session_id: 'ses-short', steps: [] }],
		} as never);
		await expect(listRuntimeSessions()).rejects.toThrow('Invalid runtime session identity');

		invokeMock.mockResolvedValueOnce({
			...resumeResponse(),
			messages: [
				{
					message_id: 'msg-00000000000000000000000000000006',
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
			...resumeResponse(sessionId),
			messages: [resumeMessage(stepId, sessionId)],
			steps: [resumeStep(stepId, sessionId)],
		};
		invokeMock.mockResolvedValueOnce(response as never);
		const mapped = await getSessionForResume({ sessionId });
		expect(mapped).toMatchObject({
			session: { sessionId },
			messages: [{ messageId: stepId, sessionId }],
			steps: [{ stepId, sessionId, step_number: 1 }],
		});
		expect(mapped.messages[0]).not.toHaveProperty('message_id');
		expect(mapped.messages[0]).not.toHaveProperty('session_id');
		expect(mapped.messages[0]).toHaveProperty('tool_call_id', null);
		expect(mapped.steps[0]).not.toHaveProperty('step_id');
		expect(mapped.steps[0]).not.toHaveProperty('session_id');
		expect(mapped.steps[0]).toHaveProperty('tool_call_id', null);

		invokeMock.mockResolvedValueOnce({ ...response, steps: [] } as never);
		await expect(getSessionForResume({ sessionId })).rejects.toThrow(
			'Invalid resumed message identity',
		);
	});

	it('owns rollback and active session lifecycle commands', async () => {
		const request = { sessionId: 'ses-d8a361dbfde93b45612fce5bf38f7f2c' };
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
		const request = { sessionId: 'ses-00000000000000000000000000000004' };
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
		const response = resumeResponse('ses-00000000000000000000000000000007');
		invokeMock.mockResolvedValue(response as never);

		await expect(
			getSessionForResume({ sessionId: 'ses-00000000000000000000000000000007' }),
		).resolves.toEqual(rendererResumeResponse('ses-00000000000000000000000000000007'));
	});
});
