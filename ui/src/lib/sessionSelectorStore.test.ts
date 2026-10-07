import { describe, expect, it, vi } from 'vitest';
import type { Readable } from 'svelte/store';
import { writable } from 'svelte/store';
import {
	initialSessionState,
	SessionReducer,
	type SessionMessage,
	type SessionReducerState,
} from './sessionReducer.ts';
import { createEqualityGatedSessionSelectorStore } from './sessionReducer/selectorStore.ts';

describe('session selector stores', () => {
	it('does not notify a sessions selector for an unrelated reducer action', () => {
		const root = writable(initialSessionState);
		const reducer = new SessionReducer(initialSessionState, root);
		const listener = vi.fn();
		const unsubscribe = createEqualityGatedSessionSelectorStore(
			root,
			(state) => state.sessions,
		).subscribe(listener);

		reducer.dispatch({
			type: 'session/usage-live',
			sessionId: 'ses-1',
			call: { call_kind: 'agent', total_tokens: 1 },
		});

		expect(listener).toHaveBeenCalledTimes(1);
		expect(listener).toHaveBeenLastCalledWith(initialSessionState.sessions);
		unsubscribe();
	});

	it('notifies once when its selected slice changes', () => {
		const root = writable(initialSessionState);
		const reducer = new SessionReducer(initialSessionState, root);
		const listener = vi.fn();
		const unsubscribe = createEqualityGatedSessionSelectorStore(
			root,
			(state) => state.sessions,
		).subscribe(listener);

		reducer.dispatch({
			type: 'sessions/loaded',
			sessions: [{ id: 'ses-1', status: 'paused' }],
		});

		expect(listener).toHaveBeenCalledTimes(2);
		expect(listener).toHaveBeenLastCalledWith([{ id: 'ses-1', status: 'paused' }]);
		unsubscribe();
	});

	it('switches the active message slice by reference and reuses a stable empty value', () => {
		const firstMessage: SessionMessage = { id: 'msg-1', role: 'user' as const, content: 'first' };
		const secondMessage: SessionMessage = { id: 'msg-2', role: 'user' as const, content: 'second' };
		const firstMessages = [firstMessage];
		const secondMessages = [secondMessage];
		const emptyMessages: SessionMessage[] = [];
		const initial: SessionReducerState = {
			...initialSessionState,
			activeSessionId: 'ses-1',
			messages: { 'ses-1': firstMessages, 'ses-2': secondMessages },
		};
		const root = writable(initial);
		const selected = createEqualityGatedSessionSelectorStore(root, (state) => {
			const sessionId = state.activeSessionId || '_draft';
			return state.messages[sessionId] ?? emptyMessages;
		});
		const values: SessionMessage[][] = [];
		const unsubscribe = selected.subscribe((messages) => values.push(messages));

		root.set({ ...initial, activeSessionId: 'ses-2' });
		expect(values).toHaveLength(2);
		expect(values[0]).toBe(firstMessages);
		expect(values[1]).toBe(secondMessages);

		root.set({ ...initial, activeSessionId: 'ses-missing' });
		const emptySelection = values.at(-1);
		root.set({ ...initial, activeSessionId: null, runEndNotice: { sessionId: 'ses-3', status: 'error', reason: 'x' } });
		expect(values).toHaveLength(3);
		expect(emptySelection).toBe(emptyMessages);
		expect(values.at(-1)).toBe(emptyMessages);
		unsubscribe();
	});

	it('does not notify when the selector returns the same reference', () => {
		const root = writable(initialSessionState);
		const listener = vi.fn();
		const unsubscribe = createEqualityGatedSessionSelectorStore(
			root,
			(state) => state.sessions,
		).subscribe(listener);

		root.set({ ...initialSessionState, activeSessionId: 'ses-1' });

		expect(listener).toHaveBeenCalledTimes(1);
		unsubscribe();
	});

	it('releases the root subscription after the final selector subscriber leaves', () => {
		let subscribeCount = 0;
		let unsubscribeCount = 0;
		let state = initialSessionState;
		const root: Readable<SessionReducerState> = {
			subscribe(run) {
				subscribeCount += 1;
				run(state);
				return () => {
					unsubscribeCount += 1;
				};
			},
		};
		const selected = createEqualityGatedSessionSelectorStore(root, (value) => value.sessions);
		const first = selected.subscribe(() => {});
		const second = selected.subscribe(() => {});

		expect(subscribeCount).toBe(1);
		first();
		expect(unsubscribeCount).toBe(0);
		second();
		expect(unsubscribeCount).toBe(1);

		state = { ...state, activeSessionId: 'ses-1' };
		const third = selected.subscribe(() => {});
		expect(subscribeCount).toBe(2);
		third();
		expect(unsubscribeCount).toBe(2);
	});
});
