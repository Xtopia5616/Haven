import { describe, expect, it, vi } from 'vitest';
import {
	initialSessionState,
	SessionReducer,
	reduceSession,
	type SessionMessage,
	type SessionSummary,
} from './sessionReducer.ts';

const session = (id: string, status = 'pending'): SessionSummary => ({ id, status });

describe('SessionReducer', () => {
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
		const state = {
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '网络失败' },
		};

		const next = reduceSession(state, {
			type: 'sessions/loaded',
			sessions: [session('ses-other')],
		});

		expect(next.sessions).toEqual([session('ses-other'), session('ses-error', 'error')]);
	});

	it('clears an error only when the same session becomes busy or is left', () => {
		const state = {
			sessions: [session('ses-error', 'error')],
			activeSessionId: 'ses-error',
			error: { sessionId: 'ses-error', reason: '失败' },
		};

		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-other',
				status: 'running',
			}).error,
		).toEqual(state.error);
		expect(
			reduceSession(state, {
				type: 'session/status-updated',
				sessionId: 'ses-error',
				status: 'pending',
			}).error,
		).toBeNull();
		expect(
			reduceSession(state, { type: 'session/selected', sessionId: 'ses-other' }).error,
		).toBeNull();
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
			type: 'agent/chunk',
			kind: 'thought',
			payload: chunk,
		});
		const duplicate = reduceSession(first, {
			type: 'agent/chunk',
			kind: 'thought',
			payload: chunk,
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
});
