import { describe, expect, it, vi } from 'vitest';
import {
	DRAFT_SESSION_ID,
	initialSessionState,
	SessionReducer,
	reduceSession,
	type SessionMessage,
	type SessionReducerState,
	type SessionSummary,
} from './sessionReducer.ts';
import type { SessionStatus } from './contracts/generatedCommands.ts';

const session = (sessionId: string, status: SessionStatus = 'pending'): SessionSummary => ({
	sessionId,
	status,
});
const stateWith = (partial: Partial<SessionReducerState>): SessionReducerState => ({
	...initialSessionState,
	...partial,
});

describe('SessionReducer', () => {
	it('clears session projections while preserving the draft and non-session interactions', () => {
		const sessionId = 'ses-00000000000000000000000000000041';
		const sessionMessage = {
			id: 'msg-00000000000000000000000000000041',
			role: 'assistant' as const,
			content: 'session transcript',
		};
		const draftMessage = {
			id: 'msg-00000000000000000000000000000042',
			role: 'user' as const,
			content: 'draft input',
		};
		const sessionInteraction = {
			requestId: 'step-00000000000000000000000000000041',
			sessionId,
			owner: { kind: 'session' as const, sessionId },
			kind: 'ask' as const,
			status: 'pending' as const,
			options: [],
			createdAt: '',
		};
		const scheduledInteraction = {
			requestId: 'conf-00000000000000000000000000000041',
			owner: {
				kind: 'scheduled_tool_run' as const,
				toolRunId: 'toolrun-00000000000000000000000000000041',
			},
			kind: 'scheduled_confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'scheduled_tool',
			summary: 'Run scheduled operation',
			permissionKey: 'tool.scheduled_tool',
			createdAt: '',
		};
		const appInteraction = {
			requestId: 'conf-00000000000000000000000000000042',
			owner: { kind: 'app_command' as const },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			createdAt: '',
		};
		const state = stateWith({
			sessions: [session(sessionId)],
			activeSessionId: sessionId,
			messages: { [sessionId]: [sessionMessage], [DRAFT_SESSION_ID]: [draftMessage] },
			interactions: {
				[sessionInteraction.requestId]: sessionInteraction,
				[scheduledInteraction.requestId]: scheduledInteraction,
				[appInteraction.requestId]: appInteraction,
			},
			tokenStats: {
				[sessionId]: {
					promptTokens: 1,
					completionTokens: 1,
					totalTokens: 2,
					cumulativePromptTokens: 1,
					cumulativeCompletionTokens: 1,
					cumulativeTotalTokens: 2,
					costUsd: 0.01,
					cumulativeCostUsd: 0.01,
					contextWindow: 128_000,
					model: 'test-model',
				},
			},
			llmUsage: { [sessionId]: [{ usageId: 'usage-00000000000000000000000000000041' }] },
			replay: {
				eventSequenceBySession: { [sessionId]: ['1'] },
				chunkSeqByMessage: { [sessionMessage.id]: 1 },
				blockIdsBySession: { [sessionId]: {} },
			},
			pendingMessageOwners: {
				[sessionMessage.id]: {
					sessionId,
				},
			},
		});

		const cleared = reduceSession(state, { type: 'sessions/cleared' });

		expect(cleared.sessions).toEqual([]);
		expect(cleared.messages).toEqual({ [DRAFT_SESSION_ID]: [draftMessage] });
		expect(cleared.interactions).toEqual({
			[scheduledInteraction.requestId]: scheduledInteraction,
			[appInteraction.requestId]: appInteraction,
		});
		expect(cleared.tokenStats).toEqual({});
		expect(cleared.llmUsage).toEqual({});
		expect(cleared.replay).toEqual({
			eventSequenceBySession: {},
			chunkSeqByMessage: {},
			blockIdsBySession: {},
		});
		expect(cleared.pendingMessageOwners).toEqual({});
	});

	it('projects a completed web search as a finalized tool message', () => {
		const next = reduceSession(initialSessionState, {
			type: 'agent/web-search',
			payload: {
				sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				phase: 'completed',
				stepNumber: 2,
				runId: 3,
				callId: 'ws_1',
				action: 'search',
				result: { count: 1 },
			},
		});
		const messages = next.messages['ses-0ecf371384ab79d597760ca41f2ba21c'];
		expect(messages).toHaveLength(1);
		expect(messages[0]).toMatchObject({
			id: 'tool-ses-0ecf371384ab79d597760ca41f2ba21c-2-3-web_search-ws_1',
			toolName: 'web_search',
			streaming: false,
		});
		expect(messages[0].content).toContain('"count":1');
	});

	it('does not resolve a confirmation through the Ask-only reducer action', () => {
		const pending = {
			requestId: 'conf-bb15d1c3636fc2a6d52e386cb419958c',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			owner: { kind: 'session' as const, sessionId: 'ses-32f23b1e00f85571a60f75dde5588074' },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			invocationStepId: 'step-00000000000000000000000000000001',
			createdAt: '2026-09-25T00:00:00Z',
		};
		const initial = stateWith({ interactions: { [pending.requestId]: pending } });

		const unchanged = reduceSession(initial, {
			type: 'session/ask-resolved',
			requestId: pending.requestId,
			response: { answer: 'approve' },
		});

		expect(unchanged).toBe(initial);
	});

	it('keeps explicit confirmation outcomes distinct and removes stale requests', () => {
		const pending = {
			requestId: 'conf-b648e1e073ddcb8231a579e118761178',
			owner: { kind: 'app_command' as const },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			createdAt: '2026-09-25T00:00:00Z',
		};
		const initial = stateWith({ interactions: { [pending.requestId]: pending } });

		const expired = reduceSession(initial, {
			type: 'session/interaction-resolution-result',
			requestId: pending.requestId,
			result: 'expired',
		});
		expect(expired.interactions[pending.requestId]?.status).toBe('expired');

		const stale = reduceSession(initial, {
			type: 'session/interaction-resolution-result',
			requestId: pending.requestId,
			result: 'stale',
		});
		expect(stale.interactions[pending.requestId]).toBeUndefined();
	});

	it('clears pending confirmations owned by a cancelled scheduled ToolRun only', () => {
		const scheduled = {
			requestId: 'conf-e3dd57e72e242ef384277968ca385a88',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			owner: {
				kind: 'scheduled_tool_run' as const,
				toolRunId: 'toolrun-2113ac173bb7c669462883a7ca897832',
			},
			kind: 'scheduled_confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'scheduled_tool',
			summary: 'Run scheduled operation',
			permissionKey: 'tool.scheduled_tool',
			createdAt: '',
		};
		const otherScheduled = {
			...scheduled,
			requestId: 'conf-f6fc3107a24b02add8d010e0a7530995',
			owner: {
				kind: 'scheduled_tool_run' as const,
				toolRunId: 'toolrun-bde27a06150dcf27f5eb46a0a0be6cf3',
			},
		};
		const sessionRequest = {
			requestId: 'conf-fdfb9b65ea4c91a392968372aa18a225',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			owner: { kind: 'session' as const, sessionId: 'ses-32f23b1e00f85571a60f75dde5588074' },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			invocationStepId: 'step-00000000000000000000000000000001',
			createdAt: '',
		};
		const initial = stateWith({
			interactions: {
				[scheduled.requestId]: scheduled,
				[otherScheduled.requestId]: otherScheduled,
				[sessionRequest.requestId]: sessionRequest,
			},
		});

		const next = reduceSession(initial, {
			type: 'session/scheduled-tool-run-cancelled',
			toolRunId: 'toolrun-2113ac173bb7c669462883a7ca897832',
		});

		expect(next.interactions[scheduled.requestId]).toBeUndefined();
		expect(next.interactions[otherScheduled.requestId]).toEqual(otherScheduled);
		expect(next.interactions[sessionRequest.requestId]).toEqual(sessionRequest);
	});

	it('reads pending interaction ids by session for stale resume protection', () => {
		const reducer = new SessionReducer();
		const request = (requestId: string, sessionId: string, status: 'pending' | 'resolved') => ({
			requestId,
			sessionId,
			owner: { kind: 'session' as const, sessionId },
			kind: 'ask' as const,
			status,
			options: [],
			createdAt: '2026-09-25T00:00:00Z',
		});
		reducer.dispatch({
			type: 'session/interaction-upserted',
			request: request('ask-live', 'ses-32f23b1e00f85571a60f75dde5588074', 'pending'),
		});
		reducer.dispatch({
			type: 'session/interaction-upserted',
			request: request('ask-resolved', 'ses-32f23b1e00f85571a60f75dde5588074', 'resolved'),
		});
		reducer.dispatch({
			type: 'session/interaction-upserted',
			request: request('ask-other', 'ses-b77487af9bb14e5ec4d60572f1248bce', 'pending'),
		});

		expect(
			reducer.getPendingInteractionRequestIds('ses-32f23b1e00f85571a60f75dde5588074'),
		).toEqual(['ask-live']);
	});

	it('hydrates and clears only requests owned by that session', () => {
		const reducer = new SessionReducer();
		const sessionRequest = {
			requestId: 'ask-session',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			owner: { kind: 'session' as const, sessionId: 'ses-32f23b1e00f85571a60f75dde5588074' },
			kind: 'ask' as const,
			status: 'pending' as const,
			options: [],
			createdAt: '',
		};
		const scheduledRequest = {
			requestId: 'conf-e3dd57e72e242ef384277968ca385a88',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			owner: {
				kind: 'scheduled_tool_run' as const,
				toolRunId: 'toolrun-00000000000000000000000000000001',
			},
			kind: 'scheduled_confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'scheduled_tool',
			summary: 'Run scheduled operation',
			permissionKey: 'tool.scheduled_tool',
			createdAt: '',
		};
		const appRequest = {
			requestId: 'conf-5fb70b6963351a7c5d21b60fb79f7e34',
			owner: { kind: 'app_command' as const },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			createdAt: '',
		};
		for (const request of [sessionRequest, scheduledRequest, appRequest])
			reducer.dispatch({ type: 'session/interaction-upserted', request });

		reducer.dispatch({
			type: 'session/interactions-hydrated',
			sessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			requests: [],
		});
		expect(reducer.snapshot().interactions).toEqual({
			[scheduledRequest.requestId]: scheduledRequest,
			[appRequest.requestId]: appRequest,
		});

		reducer.dispatch({ type: 'sessions/cleared' });
		expect(reducer.snapshot().interactions).toEqual({
			[scheduledRequest.requestId]: scheduledRequest,
			[appRequest.requestId]: appRequest,
		});
	});

	it('keeps identical terminal run-end projections from notifying selectors again', () => {
		const state = stateWith({
			sessions: [
				{
					...session('ses-bddd30ebf7650ec7be092600cb64081d', 'completed'),
					waitingReason: null,
				},
			],
			activeSessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
			runEndNotice: {
				sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
				status: 'completed',
				reason: '已结束',
			},
		});

		expect(
			reduceSession(state, {
				type: 'session/run-ended',
				sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
				status: 'completed',
				reason: '已结束',
			}),
		).toBe(state);
	});

	it('owns normalized per-session error reasons and ignores duplicate or blank updates', () => {
		const reducer = new SessionReducer();
		reducer.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: 'ses-c36288e603571a31e5c831d7da869434',
			reason: '  网络失败  ',
		});
		reducer.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: 'ses-bf789d6e0fcd7432cca7b5bc32496866',
			reason: '超时',
		});

		const unchanged = reducer.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: 'ses-c36288e603571a31e5c831d7da869434',
			reason: '网络失败',
		});
		reducer.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: 'ses-c36288e603571a31e5c831d7da869434',
			reason: '   ',
		});

		expect(unchanged).toBe(reducer.snapshot());
		expect(reducer.getSessionErrorReason('ses-c36288e603571a31e5c831d7da869434')).toBe(
			'网络失败',
		);
		expect(reducer.getSessionErrorReason('ses-bf789d6e0fcd7432cca7b5bc32496866')).toBe('超时');
		expect(reducer.getSessionErrorReason('')).toBe('');

		reducer.dispatch({
			type: 'session/error-reason-forgotten',
			sessionId: 'ses-c36288e603571a31e5c831d7da869434',
		});
		reducer.dispatch({
			type: 'session/error-reason-forgotten',
			sessionId: 'ses-c36288e603571a31e5c831d7da869434',
		});
		expect(reducer.getSessionErrorReason('ses-c36288e603571a31e5c831d7da869434')).toBe('');
		expect(reducer.getSessionErrorReason('ses-bf789d6e0fcd7432cca7b5bc32496866')).toBe('超时');
	});

	it('keeps usage projections scoped to their session', () => {
		const otherStats = {
			promptTokens: 90,
			completionTokens: 9,
			totalTokens: 99,
			cumulativePromptTokens: 90,
			cumulativeCompletionTokens: 9,
			cumulativeTotalTokens: 99,
			costUsd: null,
			cumulativeCostUsd: null,
			contextWindow: null,
			model: 'other-model',
		};
		const otherCall = { call_kind: 'agent' as const, total_tokens: 99 };
		const state = stateWith({
			activeSessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
			tokenStats: { 'ses-fb38531de0466990f46b178209103744': otherStats },
			llmUsage: { 'ses-fb38531de0466990f46b178209103744': [otherCall] },
		});
		const activeStats = {
			promptTokens: 11,
			completionTokens: 7,
			totalTokens: 18,
			cumulativePromptTokens: 11,
			cumulativeCompletionTokens: 7,
			cumulativeTotalTokens: 18,
			costUsd: null,
			cumulativeCostUsd: null,
			contextWindow: null,
			model: 'active-model',
		};
		const activeCall = { call_kind: 'agent' as const, total_tokens: 18 };

		const next = reduceSession(state, {
			type: 'session/usage-live',
			sessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
			stats: activeStats,
			call: activeCall,
		});

		expect(next.tokenStats[next.activeSessionId!]).toMatchObject(activeStats);
		expect(next.llmUsage[next.activeSessionId!]).toEqual([activeCall]);
		expect(next.tokenStats['ses-fb38531de0466990f46b178209103744']).toBe(otherStats);
		expect(next.llmUsage['ses-fb38531de0466990f46b178209103744']).toBe(
			state.llmUsage['ses-fb38531de0466990f46b178209103744'],
		);
	});

	it('does not let an unrelated background session hijack a fresh draft', () => {
		const state = reduceSession(initialSessionState, {
			type: 'session/created',
			sessionId: 'ses-ffe697a25c453d07bf0c9571521df2b7',
			freshStart: true,
			adoptedDraft: false,
		});

		expect(state.activeSessionId).toBeNull();
	});

	it('selects the session that adopted a pending fresh draft', () => {
		const state = reduceSession(initialSessionState, {
			type: 'session/created',
			sessionId: 'ses-e1b5285e36ed4e7f4fbcd3d0a41a822c',
			freshStart: true,
			adoptedDraft: true,
		});

		expect(state.activeSessionId).toBe('ses-e1b5285e36ed4e7f4fbcd3d0a41a822c');
	});

	it('preserves an active error session when a list refresh omits it', () => {
		const state = stateWith({
			sessions: [session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error')],
			activeSessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			runEndNotice: {
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'error',
				reason: '网络失败',
			},
		});

		const next = reduceSession(state, {
			type: 'sessions/loaded',
			sessions: [session('ses-fb38531de0466990f46b178209103744')],
		});

		expect(next.sessions).toEqual([
			session('ses-fb38531de0466990f46b178209103744'),
			session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error'),
		]);
	});

	it('clears an error run-end notice when the session resumes or is left', () => {
		const state = stateWith({
			sessions: [session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error')],
			activeSessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			runEndNotice: {
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'error',
				reason: '失败',
			},
		});

		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-fb38531de0466990f46b178209103744',
				status: 'running',
			}).runEndNotice,
		).toEqual(state.runEndNotice);
		expect(
			reduceSession(state, {
				type: 'session/selected',
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			}).runEndNotice,
		).toEqual(state.runEndNotice);
		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'pending',
			}).runEndNotice,
		).toBeNull();
		const left = reduceSession(state, {
			type: 'session/selected',
			sessionId: 'ses-fb38531de0466990f46b178209103744',
		});
		expect(left.runEndNotice).toBeNull();
	});

	it('clears the current error when a newly created session is activated', () => {
		const state = stateWith({
			sessions: [session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error')],
			activeSessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			runEndNotice: {
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'error',
				reason: '失败',
			},
		});

		const next = reduceSession(state, {
			type: 'session/created',
			sessionId: 'ses-9f180753744c3a8bbba27df189d6cd41',
			freshStart: true,
			adoptedDraft: true,
		});

		expect(next.activeSessionId).toBe('ses-9f180753744c3a8bbba27df189d6cd41');
		expect(next.runEndNotice).toBeNull();
	});

	it('preserves the current error when an unrelated background session is created', () => {
		const state = stateWith({
			sessions: [session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error')],
			activeSessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			runEndNotice: {
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'error',
				reason: '失败',
			},
		});

		const next = reduceSession(state, {
			type: 'session/created',
			sessionId: 'ses-ffe697a25c453d07bf0c9571521df2b7',
			freshStart: true,
			adoptedDraft: false,
		});

		expect(next.activeSessionId).toBe('ses-1a4ee0ce88355110073c1a46637b89bc');
		expect(next.runEndNotice).toEqual(state.runEndNotice);
	});

	it('clears the current error when the active session is cleared or deleted', () => {
		const state = stateWith({
			sessions: [
				session('ses-1a4ee0ce88355110073c1a46637b89bc', 'error'),
				session('ses-fb38531de0466990f46b178209103744'),
			],
			activeSessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
			runEndNotice: {
				sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
				status: 'error',
				reason: '失败',
			},
		});

		const cleared = reduceSession(state, { type: 'session/cleared' });
		expect(cleared.activeSessionId).toBeNull();
		expect(cleared.runEndNotice).toBeNull();

		const deleted = reduceSession(state, {
			type: 'session/deleted',
			sessionId: 'ses-1a4ee0ce88355110073c1a46637b89bc',
		});
		expect(deleted.sessions).toEqual([session('ses-fb38531de0466990f46b178209103744')]);
		expect(deleted.activeSessionId).toBeNull();
		expect(deleted.runEndNotice).toBeNull();
	});

	it('preserves an active terminal reason when a live-session refresh omits it', () => {
		const state = stateWith({
			sessions: [session('ses-810b085358816d1bc83caf2eb4f45ff1', 'completed')],
			activeSessionId: 'ses-810b085358816d1bc83caf2eb4f45ff1',
			runEndNotice: {
				sessionId: 'ses-810b085358816d1bc83caf2eb4f45ff1' as const,
				status: 'completed' as const,
				reason: '用户主动结束会话',
			},
		});

		const next = reduceSession(state, {
			type: 'sessions/loaded',
			sessions: [session('ses-fb38531de0466990f46b178209103744')],
		});

		expect(next.sessions).toContainEqual(
			session('ses-810b085358816d1bc83caf2eb4f45ff1', 'completed'),
		);
		expect(next.runEndNotice?.reason).toBe('用户主动结束会话');
	});

	it('notifies subscribers after every dispatch', () => {
		const reducer = new SessionReducer();
		const listener = vi.fn();
		const dispose = reducer.subscribe(listener);

		reducer.dispatch({
			type: 'session/title-updated',
			sessionId: 'ses-00000000000000000000000000000001',
			title: '研究',
		});
		dispose();
		reducer.dispatch({ type: 'session/cleared' });

		expect(listener).toHaveBeenCalledTimes(2);
	});

	it('moves and reconciles an optimistic message by id when a session is created', () => {
		const optimistic: SessionMessage = {
			id: 'u-optimistic',
			role: 'user' as const,
			content: '你好',
		};
		const withDraft = reduceSession(initialSessionState, {
			type: 'session/messages/optimistic-added',
			sessionId: '_draft',
			message: optimistic,
		});

		const accepted = reduceSession(withDraft, {
			type: 'session/messages/accepted',
			fromSessionId: '_draft',
			toSessionId: 'ses-e81537106812fb446551d3a7ec775e7e',
			optimisticMessageId: optimistic.id,
			persistedMessageId: 'msg-0000000000000000000000000000007b',
		});

		expect(accepted.messages?._draft).toEqual([]);
		expect(accepted.messages?.['ses-e81537106812fb446551d3a7ec775e7e']).toEqual([
			{
				...optimistic,
				id: 'msg-0000000000000000000000000000007b',
				received: true,
				steering: false,
			},
		]);
		expect(accepted.pendingMessageOwners?.[optimistic.id]).toBeUndefined();
	});

	it('keeps pending ownership through draft adoption and clears the migrated bubble on rejection', () => {
		const draftMessage: SessionMessage = {
			id: 'u-pending-draft',
			role: 'user',
			content: 'voice input',
		};
		const sessionId = 'ses-00000000000000000000000000000051';
		const pending = reduceSession(initialSessionState, {
			type: 'session/messages/optimistic-added',
			sessionId: DRAFT_SESSION_ID,
			message: draftMessage,
		});
		const adopted = reduceSession(pending, {
			type: 'session/messages/adopt-draft',
			sessionId,
		});

		expect(adopted.pendingMessageOwners[draftMessage.id]).toEqual({ sessionId });
		const rejected = reduceSession(adopted, {
			type: 'session/messages/rejected',
			sessionId: DRAFT_SESSION_ID,
			messageId: draftMessage.id,
		});

		expect(rejected.messages[sessionId]).toEqual([]);
		expect(rejected.pendingMessageOwners[draftMessage.id]).toBeUndefined();
	});

	it('uses canonical supplement ids and distinct renderer keys for injected tool cards', () => {
		const sessionId = 'ses-00000000000000000000000000000071';
		const peerSupplement = {
			sessionId,
			additionalContext: 'peer context',
			stepNumber: 1,
			runId: 1,
			supplementId: 'msg-00000000000000000000000000000071',
			injectSource: 'cross_session' as const,
			eventSequence: 1,
		};
		const withPeerCard = reduceSession(initialSessionState, {
			type: 'agent/supplement',
			payload: peerSupplement,
		});
		const peerCard = withPeerCard.messages[sessionId]?.[0];
		expect(peerCard).toMatchObject({
			id: peerSupplement.supplementId,
			presentationKey: `peer-mail:${peerSupplement.supplementId}`,
		});
		expect(
			reduceSession(withPeerCard, { type: 'agent/supplement', payload: peerSupplement }).messages[
				sessionId
			],
		).toHaveLength(1);

		const toolRunId = 'toolrun-00000000000000000000000000000071';
		const toolRunSupplement = {
			sessionId,
			additionalContext: `[Background tool run result]\ntool_run_id: ${toolRunId}\nstatus: completed`,
			stepNumber: 2,
			runId: 2,
			messageId: 'msg-00000000000000000000000000000072',
			supplementId: 'msg-00000000000000000000000000000072',
			injectSource: 'tool_run_result' as const,
			eventSequence: 2,
		};
		const withToolRunCard = reduceSession(withPeerCard, {
			type: 'agent/supplement',
			payload: toolRunSupplement,
		});
		expect(withToolRunCard.messages[sessionId]?.[1]).toMatchObject({
			id: toolRunSupplement.supplementId,
			presentationKey: `tool-run-result:${toolRunSupplement.supplementId}:${toolRunId}`,
			toolName: 'tool_runs.inspect',
		});
	});

	it('merges resume data by stable ids while retaining an in-flight stream', () => {
		const state: typeof initialSessionState = {
			...initialSessionState,
			messages: {
				'ses-d628ca764938045bf3f3194d0ff8893a': [
					{
						id: 'step-82ef0e384f975888083f77d13d14a4fe',
						type: 'tool',
						content: '',
						streaming: true,
					},
					{
						id: 'stale-final',
						role: 'assistant' as const,
						content: '旧内容',
						streaming: false,
					},
				],
			},
		};

		const next = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
			messages: [
				{
					id: 'step-82ef0e384f975888083f77d13d14a4fe',
					type: 'tool',
					content: '数据库尚未写完',
					streaming: false,
				},
				{
					id: 'msg-ac1a07415ae294c58298ee88de671d65',
					role: 'assistant' as const,
					content: '已保存',
					streaming: false,
				},
			],
			preserveStreamingOnly: true,
		});

		expect(next.messages?.['ses-d628ca764938045bf3f3194d0ff8893a']).toEqual([
			{
				id: 'step-82ef0e384f975888083f77d13d14a4fe',
				type: 'tool',
				content: '',
				streaming: true,
			},
			{
				id: 'msg-ac1a07415ae294c58298ee88de671d65',
				role: 'assistant' as const,
				content: '已保存',
				streaming: false,
			},
		]);
	});

	it('preserves and reanchors pending steering during streaming-only resume', () => {
		const state: typeof initialSessionState = {
			...initialSessionState,
			messages: {
				'ses-d628ca764938045bf3f3194d0ff8893a': [
					{
						id: 'msg-00000000000000000000000000000001',
						role: 'user' as const,
						content: 'hi',
					},
					{
						id: 'msg-00000000000000000000000000000002',
						role: 'user' as const,
						content: '补充',
						steering: true,
					},
				],
			},
		};

		const next = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
			messages: [
				{
					id: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
				},
				{
					id: 'msg-00000000000000000000000000000002',
					role: 'user' as const,
					content: '补充',
				},
				{
					id: 'msg-581584b22ed8f37635989297798f2af0',
					role: 'assistant' as const,
					content: '继续处理',
				},
			],
			preserveStreamingOnly: true,
		});

		expect(
			next.messages?.['ses-d628ca764938045bf3f3194d0ff8893a'].map((message) => message.id),
		).toEqual([
			'msg-00000000000000000000000000000001',
			'msg-581584b22ed8f37635989297798f2af0',
			'msg-00000000000000000000000000000002',
		]);
		expect(next.messages?.['ses-d628ca764938045bf3f3194d0ff8893a'][2]).toMatchObject({
			id: 'msg-00000000000000000000000000000002',
			steering: true,
		});
	});

	it('does not drop a live confirmation when resume snapshot is behind the event', () => {
		const pending = {
			requestId: 'conf-79e4b2f3233fd39bef539bb1aeaf517b',
			sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
			owner: { kind: 'session' as const, sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a' },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			invocationStepId: 'step-00000000000000000000000000000001',
			createdAt: '2026-09-20T00:00:00Z',
		};
		const state = { ...initialSessionState, interactions: { [pending.requestId]: pending } };

		const kept = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [],
			interactions: [],
			preserveRequestIds: [pending.requestId],
		});

		expect(kept.interactions?.[pending.requestId]).toEqual(pending);

		const resolved = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [],
			interactions: [{ ...pending, status: 'resolved' }],
			preserveRequestIds: [pending.requestId],
		});
		expect(resolved.interactions?.[pending.requestId]?.status).toBe('resolved');
	});

	it('composes resume interaction preservation with restored and live usage', () => {
		const pending = {
			requestId: 'conf-1520789359e0e946f7617ee3e901a450',
			sessionId: 'ses-7d23f8e9ee6f01342ee67c597bad08a4',
			owner: { kind: 'session' as const, sessionId: 'ses-7d23f8e9ee6f01342ee67c597bad08a4' },
			kind: 'confirm' as const,
			status: 'pending' as const,
			options: [],
			toolName: 'run_command',
			riskLevel: 'high' as const,
			summary: 'Run protected operation',
			toolIndex: 0,
			invocationStepId: 'step-00000000000000000000000000000001',
			createdAt: '2026-09-20T00:00:00Z',
		};
		const state = { ...initialSessionState, interactions: { [pending.requestId]: pending } };

		const resumed = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [
				{
					id: 'msg-da2427f68ad3025dae11e59d08bda3b3',
					role: 'assistant' as const,
					content: '已恢复',
				},
			],
			interactions: [],
			preserveRequestIds: [pending.requestId],
			usage: {
				prompt_tokens: 13,
				completion_tokens: 5,
				total_tokens: 18,
				cached_tokens: 2,
				cache_creation_tokens: 1,
				context_window: 128,
				cost_usd: 0.02,
				has_cost: true,
			},
			llmUsage: [{ call_kind: 'agent', total_tokens: 18 }],
		});

		expect(resumed.messages[pending.sessionId]).toEqual([
			{
				id: 'msg-da2427f68ad3025dae11e59d08bda3b3',
				role: 'assistant' as const,
				content: '已恢复',
			},
		]);
		expect(resumed.interactions[pending.requestId]).toEqual(pending);
		expect(resumed.tokenStats[pending.sessionId]).toMatchObject({
			cumulativeTotalTokens: 18,
			restored: true,
		});
		expect(resumed.llmUsage[pending.sessionId]).toEqual([
			{ call_kind: 'agent', total_tokens: 18 },
		]);

		const live = reduceSession(resumed, {
			type: 'session/usage-live',
			sessionId: pending.sessionId,
			stats: {
				promptTokens: 7,
				completionTokens: 3,
				totalTokens: 10,
				cumulativePromptTokens: 20,
				cumulativeCompletionTokens: 8,
				cumulativeTotalTokens: 28,
				costUsd: null,
				cumulativeCostUsd: null,
				contextWindow: 128,
				model: 'live-model',
			},
			call: { call_kind: 'agent', total_tokens: 10 },
		});

		expect(live.tokenStats[pending.sessionId]).toMatchObject({
			promptTokens: 7,
			restored: false,
			lastUpdated: expect.any(Number),
		});
		expect(live.llmUsage[pending.sessionId]).toEqual([
			{ call_kind: 'agent', total_tokens: 18 },
			{ call_kind: 'agent', total_tokens: 10 },
		]);
	});

	it('deduplicates replayed chunks and sequenced ToolCall events', () => {
		const chunk = {
			sessionId: 'ses-09a5664ab5098db158e293a1c9f97a52',
			delta: '思考',
			stepNumber: 1,
			runId: 2,
			messageId: 'step-db57142e6c7eae1b75511b732e35f0db',
			seq: 7,
		} as const;
		const first = reduceSession(initialSessionState, {
			type: 'agent/chunks',
			chunks: [{ kind: 'thought', payload: chunk }],
		});
		const duplicate = reduceSession(first, {
			type: 'agent/chunks',
			chunks: [{ kind: 'thought', payload: chunk }],
		});
		expect(duplicate).toBe(first);
		expect(duplicate.messages?.['ses-09a5664ab5098db158e293a1c9f97a52']).toHaveLength(1);

		const toolCall = {
			sessionId: 'ses-09a5664ab5098db158e293a1c9f97a52',
			toolName: 'shell.run',
			input: { command: 'echo ok' },
			stepNumber: 1,
			runId: 2,
			toolCallId: 'call-1',
			toolIndex: 0,
			stepId: 'step-82ef0e384f975888083f77d13d14a4fe',
			suppressStreamedThought: false,
			silent: false,
			eventSequence: 18,
		} as const;
		const toolCallState = reduceSession(first, { type: 'agent/tool_call', payload: toolCall });
		const replayedToolCall = reduceSession(toolCallState, {
			type: 'agent/tool_call',
			payload: toolCall,
		});
		expect(replayedToolCall).toBe(toolCallState);
		expect(toolCallState.messages?.['ses-09a5664ab5098db158e293a1c9f97a52']).toHaveLength(2);
	});

	it('applies one frame of chunks with one reducer notification and preserves order', () => {
		const reducer = new SessionReducer();
		const listener = vi.fn();
		const dispose = reducer.subscribe(listener);

		reducer.dispatch({
			type: 'agent/chunks',
			chunks: [
				{
					kind: 'reasoning',
					msgType: 'reasoning',
					payload: {
						sessionId: 'ses-0ee5edd563b411bb4eb827e9afb75e7a',
						delta: '先',
						stepNumber: 1,
						runId: 1,
						messageId: 'step-cc67d21c0ac1e99887e997c2d709a727',
						seq: 1,
					},
				},
				{
					kind: 'thought',
					payload: {
						sessionId: 'ses-0ee5edd563b411bb4eb827e9afb75e7a',
						delta: '后',
						stepNumber: 1,
						runId: 1,
						messageId: 'step-db57142e6c7eae1b75511b732e35f0db',
						seq: 1,
					},
				},
			],
		});
		dispose();

		expect(listener).toHaveBeenCalledTimes(2); // initial subscription + one frame
		expect(
			reducer
				.getMessages('ses-0ee5edd563b411bb4eb827e9afb75e7a')
				.map((message) => message.content),
		).toEqual(['先', '后']);
		expect(
			reduceSession(reducer.snapshot(), {
				type: 'agent/chunks',
				chunks: [
					{
						kind: 'thought',
						payload: {
							sessionId: 'ses-0ee5edd563b411bb4eb827e9afb75e7a',
							delta: '重复',
							stepNumber: 1,
							runId: 1,
							messageId: 'step-db57142e6c7eae1b75511b732e35f0db',
							seq: 1,
						},
					},
				],
			}),
		).toBe(reducer.snapshot());
	});

	it('resets stream chunks by block identity so a new run can restart its sequence', () => {
		const payload = {
			sessionId: 'ses-081488d4026a181b8306bbc3f659e473',
			delta: '旧代次',
			stepNumber: 3,
			runId: 7,
			messageId: 'step-75c85d86e148d5c016bb5a92c93aba7a',
			seq: 5,
		};
		const streamed = reduceSession(initialSessionState, {
			type: 'agent/chunks',
			chunks: [{ kind: 'thought', payload }],
		});
		const reset = reduceSession(streamed, {
			type: 'agent/stream-reset',
			payload: {
				sessionId: payload.sessionId,
				stepNumber: payload.stepNumber,
				runId: payload.runId,
				thoughtMessageId: payload.messageId,
				reasoningMessageId: 'msg-acb02c88e7d6c7e561f5b270fa631fd8',
			},
		});
		const restarted = reduceSession(reset, {
			type: 'agent/chunks',
			chunks: [{ kind: 'thought', payload: { ...payload, delta: '新代次', seq: 1 } }],
		});

		expect(reset.messages[payload.sessionId]).toEqual([]);
		expect(restarted.messages[payload.sessionId]).toEqual([
			expect.objectContaining({ id: payload.messageId, content: '新代次', streaming: true }),
		]);
		expect(restarted.replay.chunkSeqByMessage[payload.messageId]).toBe(1);
		expect(restarted.replay.blockIdsBySession[payload.sessionId]?.['3:7']).toEqual({
			thoughtId: payload.messageId,
		});
	});

	it('keeps the run-end notice aligned across list refreshes', () => {
		const running = stateWith({
			sessions: [session('ses-bddd30ebf7650ec7be092600cb64081d', 'running')],
			activeSessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
		});
		const failed = reduceSession(running, {
			type: 'session/run-ended',
			sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
			status: 'error',
			reason: '连接中断',
		});
		expect(failed.runEndNotice?.reason).toBe('连接中断');
		expect(failed.runEndNotice?.status).toBe('error');

		const recovered = reduceSession(failed, {
			type: 'session/status-updated',
			sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
			status: 'pending',
		});
		expect(recovered.runEndNotice).toBeNull();

		const completed = reduceSession(recovered, {
			type: 'session/run-ended',
			sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
			status: 'completed',
			reason: '用户主动结束会话',
		});
		const refreshed = reduceSession(completed, {
			type: 'sessions/loaded',
			sessions: [],
		});
		expect(refreshed.sessions).toContainEqual(
			session('ses-bddd30ebf7650ec7be092600cb64081d', 'completed'),
		);
		expect(refreshed.runEndNotice).toEqual({
			sessionId: 'ses-bddd30ebf7650ec7be092600cb64081d',
			status: 'completed',
			reason: '用户主动结束会话',
		});
	});

	it('keeps parallel cards that share one durable sequence', () => {
		const base = {
			sessionId: 'ses-3c6f66976600af4c8a7aa758dbcea7af',
			toolName: 'shell.run',
			input: { command: 'echo ok' },
			stepNumber: 1,
			runId: 1,
			toolCallId: 'call-1',
			toolIndex: 0,
			suppressStreamedThought: false,
			silent: false,
			eventSequence: 4,
		};
		const first = reduceSession(initialSessionState, {
			type: 'agent/tool_call',
			payload: {
				...base,
				stepId: 'step-ed4d1203d9eee442682361d4f077efd7',
				toolIndex: 0,
				toolCallId: 'call-a',
			},
		});
		const second = reduceSession(first, {
			type: 'agent/tool_call',
			payload: {
				...base,
				stepId: 'step-e23a75ffc10203f54edf15d337e0a880',
				toolIndex: 1,
				toolCallId: 'call-b',
			},
		});
		expect(
			second.messages?.['ses-3c6f66976600af4c8a7aa758dbcea7af']?.map((message) => message.id),
		).toEqual([
			'step-ed4d1203d9eee442682361d4f077efd7',
			'step-e23a75ffc10203f54edf15d337e0a880',
		]);
		const replayA = reduceSession(second, {
			type: 'agent/tool_call',
			payload: {
				...base,
				stepId: 'step-ed4d1203d9eee442682361d4f077efd7',
				toolIndex: 0,
				toolCallId: 'call-a',
			},
		});
		const replayB = reduceSession(replayA, {
			type: 'agent/tool_call',
			payload: {
				...base,
				stepId: 'step-e23a75ffc10203f54edf15d337e0a880',
				toolIndex: 1,
				toolCallId: 'call-b',
			},
		});
		expect(replayA).toBe(second);
		expect(replayB).toBe(replayA);

		const thought = reduceSession(replayB, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-3c6f66976600af4c8a7aa758dbcea7af',
				thought: 'keep',
				stepNumber: 2,
				runId: 1,
				messageId: 'step-db57142e6c7eae1b75511b732e35f0db',
				eventSequence: 4,
			},
		});
		const thoughtReplay = reduceSession(thought, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-3c6f66976600af4c8a7aa758dbcea7af',
				thought: 'changed',
				stepNumber: 2,
				runId: 1,
				messageId: 'step-db57142e6c7eae1b75511b732e35f0db',
				eventSequence: 4,
			},
		});
		expect(thoughtReplay).toBe(thought);
		expect(
			thought.messages?.['ses-3c6f66976600af4c8a7aa758dbcea7af']?.some(
				(message) => message.content === 'keep',
			),
		).toBe(true);

		const unsequenced = reduceSession(thought, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-3c6f66976600af4c8a7aa758dbcea7af',
				thought: 'snap',
				stepNumber: 3,
				runId: 1,
				messageId: 'step-f433e9a7d53cbb887b3b4ff86808d766',
			},
		});
		const unsequencedAgain = reduceSession(unsequenced, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-3c6f66976600af4c8a7aa758dbcea7af',
				thought: 'snap-again',
				stepNumber: 3,
				runId: 1,
				messageId: 'step-f433e9a7d53cbb887b3b4ff86808d766',
			},
		});
		expect(unsequencedAgain).not.toBe(unsequenced);
		expect(
			unsequencedAgain.messages?.['ses-3c6f66976600af4c8a7aa758dbcea7af']?.some(
				(message) => message.content === 'snap-again',
			),
		).toBe(true);
	});
});
