import { describe, expect, it, vi } from 'vitest';
import {
	initialSessionState,
	SessionReducer,
	reduceSession,
	resumeInteractions,
	type SessionMessage,
	type SessionReducerState,
	type SessionSummary,
} from './sessionReducer.ts';

const session = (id: string, status = 'pending'): SessionSummary => ({ id, status });
const stateWith = (partial: Partial<SessionReducerState>): SessionReducerState => ({
	...initialSessionState,
	...partial,
});

describe('resume interaction normalization', () => {
	it('keeps snake_case wire and camelCase compatibility rows on the resume path', () => {
		const requests = resumeInteractions({
			interactions: [
				{
					id: 'conf-wire',
					session_id: 'ses-wire',
					kind: 'confirm',
					status: 'pending',
					prompt: 'Confirm this action?',
					options: [],
					tool_name: 'run_command',
					risk_level: 'high',
					invocation_step_id: 'step-1',
					action_index: 2,
					tool_call_id: 'call-1',
					created_at: '2026-09-25T00:00:00Z',
					expires_at: '2026-09-25T00:01:00Z',
				},
				{
					id: 'ask-legacy',
					sessionId: 'ses-legacy',
					kind: 'ask',
					status: 'pending',
					prompt: 'Choose one',
					options: ['A', 'B'],
					createdAt: '2026-09-25T00:02:00Z',
				},
				{ id: 'missing-session', kind: 'ask', status: 'pending' },
				{ id: 'invalid-session', session_id: 7, kind: 'ask', status: 'pending' },
			],
		});

		expect(requests).toEqual([
			{
				id: 'conf-wire',
				sessionId: 'ses-wire',
				kind: 'confirm',
				status: 'pending',
				prompt: 'Confirm this action?',
				options: [],
				toolName: 'run_command',
				riskLevel: 'high',
				invocationStepId: 'step-1',
				actionIndex: 2,
				toolCallId: 'call-1',
				createdAt: '2026-09-25T00:00:00Z',
				expiresAt: '2026-09-25T00:01:00Z',
			},
			{
				id: 'ask-legacy',
				sessionId: 'ses-legacy',
				kind: 'ask',
				status: 'pending',
				prompt: 'Choose one',
				options: ['A', 'B'],
				createdAt: '2026-09-25T00:02:00Z',
			},
		]);
	});

	it('returns an empty list for a malformed resume envelope', () => {
		expect(resumeInteractions(null)).toEqual([]);
		expect(resumeInteractions({ interactions: {} })).toEqual([]);
	});
});

describe('SessionReducer', () => {
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
			activeSessionId: 'ses-active',
			tokenStats: { 'ses-other': otherStats },
			llmUsage: { 'ses-other': [otherCall] },
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
			sessionId: 'ses-active',
			stats: activeStats,
			call: activeCall,
		});

		expect(next.tokenStats[next.activeSessionId!]).toMatchObject(activeStats);
		expect(next.llmUsage[next.activeSessionId!]).toEqual([activeCall]);
		expect(next.tokenStats['ses-other']).toBe(otherStats);
		expect(next.llmUsage['ses-other']).toBe(state.llmUsage['ses-other']);
	});

	it('does not let an unrelated background session hijack a fresh draft', () => {
		const state = reduceSession(initialSessionState, {
			type: 'session/created',
			sessionId: 'ses-background',
			freshStart: true,
			adoptedDraft: false,
		});

		expect(state.activeSessionId).toBeNull();
	});

	it('selects the session that adopted a pending fresh draft', () => {
		const state = reduceSession(initialSessionState, {
			type: 'session/created',
			sessionId: 'ses-voice',
			freshStart: true,
			adoptedDraft: true,
		});

		expect(state.activeSessionId).toBe('ses-voice');
	});

	it('preserves an active error session when a list refresh omits it', () => {
		const state = stateWith({
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '网络失败' },
		});

		const next = reduceSession(state, {
			type: 'sessions/loaded',
			sessions: [session('ses-other')],
		});

		expect(next.sessions).toEqual([session('ses-other'), session('ses-error', 'error')]);
	});

	it('clears an error only when the same session becomes busy or is left', () => {
		const state = stateWith({
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '失败' },
			termination: { sessionId: 'ses-error', status: 'error', reason: '失败' },
		});

		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-other',
				status: 'running',
			}).error,
		).toEqual(state.error);
		expect(
			reduceSession(state, { type: 'session/selected', sessionId: 'ses-error' }).error,
		).toEqual(state.error);
		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-error',
				status: 'pending',
			}).error,
		).toBeNull();
		const left = reduceSession(state, { type: 'session/selected', sessionId: 'ses-other' });
		expect(left.error).toBeNull();
		expect(left.termination).toBeNull();
	});

	it('clears the current error when a newly created session is activated', () => {
		const state = stateWith({
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '失败' },
			termination: { sessionId: 'ses-error', status: 'error', reason: '失败' },
		});

		const next = reduceSession(state, {
			type: 'session/created',
			sessionId: 'ses-new',
			freshStart: true,
			adoptedDraft: true,
		});

		expect(next.activeSessionId).toBe('ses-new');
		expect(next.error).toBeNull();
		expect(next.termination).toBeNull();
	});

	it('preserves the current error when an unrelated background session is created', () => {
		const state = stateWith({
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '失败' },
			termination: { sessionId: 'ses-error', status: 'error', reason: '失败' },
		});

		const next = reduceSession(state, {
			type: 'session/created',
			sessionId: 'ses-background',
			freshStart: true,
			adoptedDraft: false,
		});

		expect(next.activeSessionId).toBe('ses-error');
		expect(next.error).toEqual(state.error);
		expect(next.termination).toEqual(state.termination);
	});

	it('clears the current error when the active session is cleared or deleted', () => {
		const state = stateWith({
			sessions: [session('ses-error', 'error'), session('ses-other')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '失败' },
			termination: { sessionId: 'ses-error', status: 'error', reason: '失败' },
		});

		const cleared = reduceSession(state, { type: 'session/cleared' });
		expect(cleared.activeSessionId).toBeNull();
		expect(cleared.error).toBeNull();
		expect(cleared.termination).toBeNull();

		const deleted = reduceSession(state, { type: 'session/deleted', sessionId: 'ses-error' });
		expect(deleted.sessions).toEqual([session('ses-other')]);
		expect(deleted.activeSessionId).toBeNull();
		expect(deleted.error).toBeNull();
		expect(deleted.termination).toBeNull();
	});

	it('preserves an active terminal reason when a live-session refresh omits it', () => {
		const state = stateWith({
			sessions: [session('ses-done', 'completed')],
			activeSessionId: 'ses-done',
			error: null,
			termination: {
				sessionId: 'ses-done' as const,
				status: 'completed' as const,
				reason: '用户主动结束会话',
			},
		});

		const next = reduceSession(state, {
			type: 'sessions/loaded',
			sessions: [session('ses-other')],
		});

		expect(next.sessions).toContainEqual(session('ses-done', 'completed'));
		expect(next.termination?.reason).toBe('用户主动结束会话');
	});

	it('notifies subscribers after every dispatch', () => {
		const reducer = new SessionReducer();
		const listener = vi.fn();
		const dispose = reducer.subscribe(listener);

		reducer.dispatch({
			type: 'session/title-updated',
			sessionId: 'ses-1',
			title: '研究',
		});
		dispose();
		reducer.dispatch({ type: 'session/cleared' });

		expect(listener).toHaveBeenCalledTimes(2);
	});

	it('moves and reconciles an optimistic message by id when a session is created', () => {
		const optimistic: SessionMessage = {
			id: 'u-optimistic',
			role: 'user',
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
			toSessionId: 'ses-created',
			optimisticId: optimistic.id,
			persistedId: 'msg-123',
		});

		expect(accepted.messages?._draft).toEqual([]);
		expect(accepted.messages?.['ses-created']).toEqual([
			{ ...optimistic, id: 'msg-123', received: true, steering: false },
		]);
		expect(accepted.optimistic?.[optimistic.id]).toEqual({
			sessionId: 'ses-created',
			messageId: 'msg-123',
			status: 'accepted',
		});
	});

	it('merges resume data by stable ids while retaining an in-flight stream', () => {
		const state: typeof initialSessionState = {
			...initialSessionState,
			messages: {
				'ses-live': [
					{ id: 'step-tool', type: 'tool', content: '', streaming: true },
					{ id: 'stale-final', role: 'assistant', content: '旧内容', streaming: false },
				],
			},
		};

		const next = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-live',
			messages: [
				{ id: 'step-tool', type: 'tool', content: '数据库尚未写完', streaming: false },
				{ id: 'msg-db', role: 'assistant', content: '已保存', streaming: false },
			],
			preserveStreamingOnly: true,
		});

		expect(next.messages?.['ses-live']).toEqual([
			{ id: 'step-tool', type: 'tool', content: '', streaming: true },
			{ id: 'msg-db', role: 'assistant', content: '已保存', streaming: false },
		]);
	});

	it('does not drop a live confirmation when resume snapshot is behind the event', () => {
		const pending = {
			id: 'conf-live',
			sessionId: 'ses-live',
			kind: 'confirm' as const,
			status: 'pending' as const,
			prompt: '需要确认',
			options: [],
			createdAt: '2026-09-20T00:00:00Z',
		};
		const state = { ...initialSessionState, interactions: { [pending.id]: pending } };

		const kept = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [],
			interactions: [],
			preserveInteractionIds: [pending.id],
		});

		expect(kept.interactions?.[pending.id]).toEqual(pending);

		const resolved = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [],
			interactions: [{ ...pending, status: 'resolved' }],
			preserveInteractionIds: [pending.id],
		});
		expect(resolved.interactions?.[pending.id]?.status).toBe('resolved');
	});

	it('composes resume interaction preservation with restored and live usage', () => {
		const pending = {
			id: 'conf-resume-live',
			sessionId: 'ses-resume-live',
			kind: 'confirm' as const,
			status: 'pending' as const,
			prompt: '允许继续吗？',
			options: [],
			createdAt: '2026-09-20T00:00:00Z',
		};
		const state = { ...initialSessionState, interactions: { [pending.id]: pending } };

		const resumed = reduceSession(state, {
			type: 'session/messages/resume-loaded',
			sessionId: pending.sessionId,
			messages: [{ id: 'msg-resumed', role: 'assistant', content: '已恢复' }],
			interactions: [],
			preserveInteractionIds: [pending.id],
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
			{ id: 'msg-resumed', role: 'assistant', content: '已恢复' },
		]);
		expect(resumed.interactions[pending.id]).toEqual(pending);
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

	it('deduplicates replayed chunks and sequenced action events', () => {
		const chunk = {
			sessionId: 'ses-replay',
			delta: '思考',
			stepNumber: 1,
			runId: 2,
			messageId: 'step-thought',
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
		expect(duplicate.messages?.['ses-replay']).toHaveLength(1);

		const action = {
			sessionId: 'ses-replay',
			toolName: 'shell.run',
			input: { command: 'echo ok' },
			stepNumber: 1,
			runId: 2,
			toolCallId: 'call-1',
			actionIndex: 0,
			stepId: 'step-tool',
			suppressStreamedThought: false,
			silent: false,
			eventSeq: 18,
		} as const;
		const actionState = reduceSession(first, { type: 'agent/action', payload: action });
		const replayedAction = reduceSession(actionState, {
			type: 'agent/action',
			payload: action,
		});
		expect(replayedAction).toBe(actionState);
		expect(actionState.messages?.['ses-replay']).toHaveLength(2);
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
						sessionId: 'ses-frame',
						delta: '先',
						stepNumber: 1,
						runId: 1,
						messageId: 'step-reasoning',
						seq: 1,
					},
				},
				{
					kind: 'thought',
					payload: {
						sessionId: 'ses-frame',
						delta: '后',
						stepNumber: 1,
						runId: 1,
						messageId: 'step-thought',
						seq: 1,
					},
				},
			],
		});
		dispose();

		expect(listener).toHaveBeenCalledTimes(2); // initial subscription + one frame
		expect(reducer.getMessages('ses-frame').map((message) => message.content)).toEqual([
			'先',
			'后',
		]);
		expect(
			reduceSession(reducer.getState(), {
				type: 'agent/chunks',
				chunks: [
					{
						kind: 'thought',
						payload: {
							sessionId: 'ses-frame',
							delta: '重复',
							stepNumber: 1,
							runId: 1,
							messageId: 'step-thought',
							seq: 1,
						},
					},
				],
			}),
		).toBe(reducer.getState());
	});

	it('resets stream chunks by block identity so a new run can restart its sequence', () => {
		const payload = {
			sessionId: 'ses-reset',
			delta: '旧代次',
			stepNumber: 3,
			runId: 7,
			messageId: 'step-thought-reset',
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
				reasoningMessageId: 'step-reasoning-reset',
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

	it('keeps lifecycle error and termination projections aligned across list refreshes', () => {
		const running = stateWith({
			sessions: [session('ses-terminal', 'running')],
			activeSessionId: 'ses-terminal',
		});
		const failed = reduceSession(running, {
			type: 'session/error-shown',
			sessionId: 'ses-terminal',
			reason: '连接中断',
		});
		expect(failed.error?.reason).toBe('连接中断');
		expect(failed.termination?.status).toBe('error');

		const recovered = reduceSession(failed, {
			type: 'session/status-updated',
			sessionId: 'ses-terminal',
			status: 'pending',
		});
		expect(recovered.error).toBeNull();
		expect(recovered.termination).toBeNull();

		const completed = reduceSession(recovered, {
			type: 'session/termination-shown',
			sessionId: 'ses-terminal',
			status: 'completed',
			reason: '用户主动结束会话',
		});
		const refreshed = reduceSession(completed, {
			type: 'sessions/loaded',
			sessions: [],
		});
		expect(refreshed.sessions).toContainEqual(session('ses-terminal', 'completed'));
		expect(refreshed.termination).toEqual({
			sessionId: 'ses-terminal',
			status: 'completed',
			reason: '用户主动结束会话',
		});
	});

	it('keeps parallel cards that share one durable sequence', () => {
		const base = {
			sessionId: 'ses-parallel',
			toolName: 'shell.run',
			input: { command: 'echo ok' },
			stepNumber: 1,
			runId: 1,
			toolCallId: 'call-1',
			actionIndex: 0,
			suppressStreamedThought: false,
			silent: false,
			eventSeq: 4,
		};
		const first = reduceSession(initialSessionState, {
			type: 'agent/action',
			payload: { ...base, stepId: 'step-a', actionIndex: 0, toolCallId: 'call-a' },
		});
		const second = reduceSession(first, {
			type: 'agent/action',
			payload: { ...base, stepId: 'step-b', actionIndex: 1, toolCallId: 'call-b' },
		});
		expect(second.messages?.['ses-parallel']?.map((message) => message.id)).toEqual([
			'step-a',
			'step-b',
		]);
		const replayA = reduceSession(second, {
			type: 'agent/action',
			payload: { ...base, stepId: 'step-a', actionIndex: 0, toolCallId: 'call-a' },
		});
		const replayB = reduceSession(replayA, {
			type: 'agent/action',
			payload: { ...base, stepId: 'step-b', actionIndex: 1, toolCallId: 'call-b' },
		});
		expect(replayA).toBe(second);
		expect(replayB).toBe(replayA);

		const thought = reduceSession(replayB, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-parallel',
				thought: 'keep',
				stepNumber: 2,
				runId: 1,
				messageId: 'step-thought',
				eventSeq: 4,
			},
		});
		const thoughtReplay = reduceSession(thought, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-parallel',
				thought: 'changed',
				stepNumber: 2,
				runId: 1,
				messageId: 'step-thought',
				eventSeq: 4,
			},
		});
		expect(thoughtReplay).toBe(thought);
		expect(
			thought.messages?.['ses-parallel']?.some((message) => message.content === 'keep'),
		).toBe(true);

		const unsequenced = reduceSession(thought, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-parallel',
				thought: 'snap',
				stepNumber: 3,
				runId: 1,
				messageId: 'step-snap',
			},
		});
		const unsequencedAgain = reduceSession(unsequenced, {
			type: 'agent/thought',
			payload: {
				sessionId: 'ses-parallel',
				thought: 'snap-again',
				stepNumber: 3,
				runId: 1,
				messageId: 'step-snap',
			},
		});
		expect(unsequencedAgain).not.toBe(unsequenced);
		expect(
			unsequencedAgain.messages?.['ses-parallel']?.some(
				(message) => message.content === 'snap-again',
			),
		).toBe(true);
	});
});
