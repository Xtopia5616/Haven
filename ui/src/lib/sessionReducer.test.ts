import { describe, expect, it, vi } from 'vitest';
import {
	initialSessionState,
	SessionReducer,
	reduceSession,
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
});
