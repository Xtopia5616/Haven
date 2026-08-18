import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
	notificationStore,
	addNotification,
	sessionMessagesStore,
	setSessionMessages,
	addSessionMessage,
	updateSessionMessages,
	truncateSessionMessages,
	adoptDraftMessages,
	moveSessionMessages,
	clearSessionMessages,
	seqLastSeen,
	pruneSeq,
	clearSeqMap,
	newMessage,
	modelStateStore,
	updateModelState,
	clearModelStateTimer,
	sessionTokenStatsStore,
	updateSessionTokenStats,
	clearSessionTokenStats,
	restoreSessionTokenStats,
	formatTokenCount,
	formatCostUsd,
} from './stores.ts';

describe('addNotification', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		notificationStore.set([]);
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('adds a notification with msg and type', () => {
		addNotification('hello', 'info');
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('hello');
		expect(items[0].type).toBe('info');
		expect(typeof items[0].id).toBe('string');
	});

	it('deduplicates identical msg+type', () => {
		addNotification('same', 'warn');
		addNotification('same', 'warn');
		addNotification('same', 'info');
		expect(get(notificationStore)).toHaveLength(2);
	});

	it('auto-removes after the duration', () => {
		addNotification('temp', 'info', 1000);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(999);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(2);
		expect(get(notificationStore)).toHaveLength(0);
	});

	it('removes only its own notification', () => {
		addNotification('a', 'info', 1000);
		addNotification('b', 'info', 5000);
		vi.advanceTimersByTime(1001);
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('b');
	});

	it('logs error notifications via logger.error', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		addNotification('boom', 'error');
		addNotification('ok', 'info');
		addNotification('oops', 'warning');
		const errorCalls = spy.mock.calls.filter((args) =>
			typeof args[0] === 'string' && args[0].includes('[ERROR]')
		);
		expect(errorCalls).toHaveLength(1);
		expect(errorCalls[0][0]).toContain('notification');
		expect(errorCalls[0][0]).toContain('boom');
		spy.mockRestore();
	});
});

describe('session message store', () => {
	/** @returns {any} */
	const storeMap = () => get(sessionMessagesStore);

	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	it('setSessionMessages replaces the list for a session', () => {
		const msgs = [{ id: '1' }];
		setSessionMessages('t1', msgs);
		expect(storeMap().t1).toBe(msgs);
	});

	it('addSessionMessage appends and preserves other sessions', () => {
		setSessionMessages('t1', [{ id: '1' }]);
		addSessionMessage('t1', { id: '2' });
		addSessionMessage('t2', { id: 'a' });
		const m = storeMap();
		expect(m.t1.map((x) => x.id)).toEqual(['1', '2']);
		expect(m.t2.map((x) => x.id)).toEqual(['a']);
	});

	it('updateSessionMessages maps existing list', () => {
		setSessionMessages('t1', [{ id: '1', done: false }]);
		updateSessionMessages('t1', (list) => list.map((x) => ({ ...x, done: true })));
		expect(storeMap().t1[0].done).toBe(true);
	});

	it('updateSessionMessages starts from empty list when absent', () => {
		updateSessionMessages('t9', (list) => [...list, { id: 'x' }]);
		expect(storeMap().t9.map((x) => x.id)).toEqual(['x']);
	});

	it('updateSessionMessages skips the write when the updater returns the same list', () => {
		setSessionMessages('t1', [{ id: '1' }]);
		const before = storeMap();
		// No-op updater (same array reference) must not replace the store map,
		// otherwise the reference-based skip would be invisible to subscribers.
		updateSessionMessages('t1', (list) => list);
		expect(storeMap()).toBe(before);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['1']);
	});

	it('updateSessionMessages still writes when the updater returns a new list', () => {
		setSessionMessages('t1', [{ id: '1' }]);
		updateSessionMessages('t1', (list) => [...list, { id: '2' }]);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['1', '2']);
	});

	it('clearSessionMessages removes the session and its seq tracking', () => {
		setSessionMessages('t1', [{ id: '1' }]);
		seqLastSeen('t1-s1', 1);
		clearSessionMessages('t1');
		expect(storeMap().t1).toBeUndefined();
		expect(seqLastSeen('t1-s1', 1)).toBe(false);
	});
});

describe('truncateSessionMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(sessionMessagesStore);

	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	const msgs = () => [
		{ id: 'a', stepNumber: 1 },
		{ id: 'b', stepNumber: 2 },
		{ id: 'c', role: 'user' },
		{ id: 'd', stepNumber: 3 },
		{ id: 'e', stepNumber: 4 },
	];

	it('drops messages at and after the target step', () => {
		setSessionMessages('t1', msgs());
		truncateSessionMessages('t1', 3);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['a', 'b', 'c']);
	});

	it('keeps everything when no message reaches the target step', () => {
		setSessionMessages('t1', msgs());
		truncateSessionMessages('t1', 99);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['a', 'b', 'c', 'd', 'e']);
	});

	it('does not cut on a user message at the target step', () => {
		// Review-mode messages: user inputs carry the stepNumber of the
		// following assistant message. Cutting ON the user message would drop
		// the user's input from the view while the backend keeps it (it was
		// persisted before the branch point). The cut lands on the first
		// non-user message at/after the target step; user messages before the
		// cut are kept, those after it belong to the discarded timeline.
		setSessionMessages('t1', [
			{ id: 'u1', role: 'user', stepNumber: 1 },
			{ id: 't1', stepNumber: 1 },
			{ id: 'o1', stepNumber: 1 },
			{ id: 'u2', role: 'user', stepNumber: 2 },
			{ id: 't2', stepNumber: 2 },
		]);
		truncateSessionMessages('t1', 2);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['u1', 't1', 'o1', 'u2']);
	});

	it('keeps the first user message when rolling back to step 1', () => {
		setSessionMessages('t1', [
			{ id: 'u1', role: 'user', stepNumber: 1 },
			{ id: 't1', stepNumber: 1 },
		]);
		truncateSessionMessages('t1', 1);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['u1']);
	});

	it('is a no-op for a missing session', () => {
		setSessionMessages('t1', msgs());
		truncateSessionMessages('nope', 3);
		expect(storeMap().t1).toHaveLength(5);
	});
});

describe('adoptDraftMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(sessionMessagesStore);

	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	it('prepends draft messages into the new session before any agent streaming', () => {
		addSessionMessage('_draft', { id: 'd1', role: 'user' });
		setSessionMessages('t1', [{ id: 'e1' }]);
		adoptDraftMessages('t1');
		const m = storeMap();
		expect(m.t1.map((x) => x.id)).toEqual(['d1', 'e1']);
		expect(m.t1[0].received).toBe(true);
		expect(m._draft).toEqual([]);
	});

	it('leaves the store untouched when there is no draft', () => {
		setSessionMessages('t1', [{ id: 'e1' }]);
		adoptDraftMessages('t1');
		expect(storeMap().t1).toHaveLength(1);
		expect(storeMap()._draft).toBeUndefined();
	});
});

describe('moveSessionMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(sessionMessagesStore);

	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	it('prepends messages from a stale session into a newly created one', () => {
		setSessionMessages('stale', [{ id: 's1', role: 'user' }, { id: 's2', role: 'user' }]);
		setSessionMessages('new', [{ id: 'n1' }]);
		moveSessionMessages('stale', 'new');
		const m = storeMap();
		expect(m.new.map((x) => x.id)).toEqual(['s1', 's2', 'n1']);
		expect(m.new.every((x) => x.role !== 'user' || x.received)).toBe(true);
		expect(m.stale).toEqual([]);
	});

	it('moves draft messages into a new session', () => {
		addSessionMessage('_draft', { id: 'd1', role: 'user' });
		moveSessionMessages('_draft', 't9');
		const m = storeMap();
		expect(m.t9.map((x) => x.id)).toEqual(['d1']);
		expect(m.t9[0].received).toBe(true);
		expect(m._draft).toEqual([]);
	});

	it('is a no-op for same-key or empty sources', () => {
		moveSessionMessages('a', 'a');
		moveSessionMessages('a', 'b');
		expect(storeMap()).toEqual({});
	});
});

describe('seqLastSeen / pruneSeq / clearSeqMap', () => {
	beforeEach(() => {
		clearSeqMap('t');
	});

	it('accepts a first sequence and rejects replays', () => {
		expect(seqLastSeen('t-s1', 1)).toBe(false);
		expect(seqLastSeen('t-s1', 1)).toBe(true);
		expect(seqLastSeen('t-s1', 2)).toBe(false);
		expect(seqLastSeen('t-s1', 2)).toBe(true);
	});

	it('treats a null sequence as not a replay', () => {
		expect(seqLastSeen('t-s1', null as any)).toBe(false);
	});

	it('pruneSeq forgets the step', () => {
		seqLastSeen('t-s1', 3);
		expect(seqLastSeen('t-s1', 3)).toBe(true);
		pruneSeq('t-s1');
		expect(seqLastSeen('t-s1', 3)).toBe(false);
	});

	it('clearSeqMap only removes keys containing the session id', () => {
		seqLastSeen('t-s1', 1);
		seqLastSeen('aaa-s1', 1);
		clearSeqMap('t');
		expect(seqLastSeen('t-s1', 1)).toBe(false);
		expect(seqLastSeen('aaa-s1', 1)).toBe(true);
	});
});

describe('newMessage', () => {
	it('builds a message with default type and voice', () => {
		const msg = newMessage({ role: 'assistant', content: 'hi' });
		expect(msg.role).toBe('assistant');
		expect(msg.content).toBe('hi');
		expect(msg.type).toBeNull();
		expect(msg.voice).toBe(false);
		expect(typeof msg.id).toBe('string');
		expect(msg.time).toBeTruthy();
	});

	it('generates unique ids', () => {
		const a = newMessage({ role: 'user', content: 'x' });
		const b = newMessage({ role: 'user', content: 'x' });
		expect(a.id).not.toBe(b.id);
	});

	it('idPrefix slots into the id between timestamp and randomness', () => {
		const msg = newMessage({ role: 'user', content: 'x', idPrefix: 'u' });
		expect(msg.id).toMatch(/^\d+-u-[a-z0-9]+$/);
	});

	it('keeps attachments and overrides time and voice', () => {
		const msg = newMessage({
			role: 'user',
			content: 'x',
			voice: true,
			time: '12:00:00',
			attachments: [{ media_type: 'image/png', data: 'a' }],
		});
		expect(msg.voice).toBe(true);
		expect(msg.time).toBe('12:00:00');
		expect(msg.attachments).toEqual([{ media_type: 'image/png', data: 'a' }]);
	});
});

describe('updateModelState', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		clearModelStateTimer();
		modelStateStore.set('ready');
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('falls back from waiting to ready after the default delay', () => {
		updateModelState('waiting');
		expect(get(modelStateStore)).toBe('waiting');
		vi.advanceTimersByTime(5000);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('falls back from streaming to ready after 2s', () => {
		updateModelState('streaming');
		vi.advanceTimersByTime(2000);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('honours a custom idle timeout', () => {
		updateModelState('waiting', { idleTimeoutMs: 100 });
		vi.advanceTimersByTime(100);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('a new state supersedes the pending idle timer', () => {
		updateModelState('waiting');
		updateModelState('streaming');
		vi.advanceTimersByTime(2000);
		// Only the 2s streaming idle timer applies; the waiting timer was cleared.
		expect(get(modelStateStore)).toBe('ready');
	});

	it('clearModelStateTimer cancels a pending idle timer', () => {
		updateModelState('waiting');
		clearModelStateTimer();
		vi.advanceTimersByTime(10000);
		expect(get(modelStateStore)).toBe('waiting');
	});

	it('stalled persists until a later state supersedes it', () => {
		// The stall indicator must NOT auto-revert: the provider may stay
		// silent for the whole idle-timeout window, and only the next chunk
		// (streaming) or a terminal session event (ready/error) clears it.
		updateModelState('stalled');
		vi.advanceTimersByTime(60000);
		expect(get(modelStateStore)).toBe('stalled');
		updateModelState('streaming');
		expect(get(modelStateStore)).toBe('streaming');
	});
});

describe('sessionTokenStatsStore', () => {
	/** @returns {any} */
	const statsMap = () => get(sessionTokenStatsStore);

	beforeEach(() => {
		sessionTokenStatsStore.set({});
	});

	it('updateSessionTokenStats inserts a new session entry', () => {
		updateSessionTokenStats('t1', { totalTokens: 5 });
		expect(statsMap().t1.totalTokens).toBe(5);
		expect(statsMap().t1.lastUpdated).toBeGreaterThan(0);
	});

	it('updateSessionTokenStats merges over the existing entry', () => {
		updateSessionTokenStats('t1', { promptTokens: 1 });
		updateSessionTokenStats('t1', { completionTokens: 2 });
		expect(statsMap().t1.promptTokens).toBe(1);
		expect(statsMap().t1.completionTokens).toBe(2);
	});

	it('updateSessionTokenStats isolates different sessions', () => {
		updateSessionTokenStats('t1', { totalTokens: 10 });
		updateSessionTokenStats('t2', { totalTokens: 20 });
		expect(statsMap().t1.totalTokens).toBe(10);
		expect(statsMap().t2.totalTokens).toBe(20);
	});

	it('clearSessionTokenStats removes only the target session', () => {
		updateSessionTokenStats('t1', { totalTokens: 10 });
		updateSessionTokenStats('t2', { totalTokens: 20 });
		clearSessionTokenStats('t1');
		expect(statsMap().t1).toBeUndefined();
		expect(statsMap().t2.totalTokens).toBe(20);
	});

	it('clearSessionTokenStats is a no-op for unknown sessions', () => {
		updateSessionTokenStats('t1', { totalTokens: 10 });
		clearSessionTokenStats('nope');
		expect(statsMap().t1.totalTokens).toBe(10);
	});

	it('updateSessionTokenStats ignores a missing session id', () => {
		updateSessionTokenStats('', { totalTokens: 10 });
		expect(statsMap()).toEqual({});
	});

	it('restoreSessionTokenStats restores cumulative counters without cost', () => {
		restoreSessionTokenStats('t1', { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150, cost_usd: 0.25, has_cost: true });
		const e = statsMap().t1;
		expect(e.cumulativePromptTokens).toBe(100);
		expect(e.cumulativeCompletionTokens).toBe(50);
		expect(e.cumulativeTotalTokens).toBe(150);
		expect(e.cumulativeCostUsd).toBe(0.25);
		expect(e.costUsd).toBeNull();
		expect(e.estimated).toBe(false);
		expect(e.restored).toBe(true);
	});

	it('restoreSessionTokenStats flags estimated totals and drops cost', () => {
		restoreSessionTokenStats('t1', { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150, cost_usd: 0.0, has_cost: false }, true);
		const e = statsMap().t1;
		expect(e.cumulativeTotalTokens).toBe(150);
		expect(e.estimated).toBe(true);
		expect(e.cumulativeCostUsd).toBeNull();
	});

	it('restoreSessionTokenStats no-ops without usage', () => {
		restoreSessionTokenStats('t1', null as any);
		expect(statsMap().t1).toBeUndefined();
	});
});

describe('formatTokenCount', () => {
	it('formats plain counts without suffix', () => {
		expect(formatTokenCount(0)).toBe('0');
		expect(formatTokenCount(999)).toBe('999');
	});

	it('formats thousands with K suffix', () => {
		expect(formatTokenCount(1234)).toBe('1.23K');
		expect(formatTokenCount(12000)).toBe('12K');
	});

	it('formats millions with M suffix', () => {
		expect(formatTokenCount(1234567)).toBe('1.23M');
	});

	it('tolerates non-numeric input', () => {
		expect(formatTokenCount(undefined as any)).toBe('0');
		expect(formatTokenCount('300' as any)).toBe('300');
	});
});

describe('formatCostUsd', () => {
	it('returns null for missing or non-finite values', () => {
		expect(formatCostUsd(null)).toBeNull();
		expect(formatCostUsd(undefined)).toBeNull();
		expect(formatCostUsd(NaN)).toBeNull();
		expect(formatCostUsd(Infinity)).toBeNull();
	});

	it('formats zero', () => {
		expect(formatCostUsd(0)).toBe('$0.00');
	});

	it('uses 4 decimals for sub-cent costs', () => {
		expect(formatCostUsd(0.00123)).toBe('$0.0012');
	});

	it('uses 3 decimals under one dollar', () => {
		expect(formatCostUsd(0.1234)).toBe('$0.123');
	});

	it('uses 2 decimals for whole dollars', () => {
		expect(formatCostUsd(1.5)).toBe('$1.50');
		expect(formatCostUsd(21)).toBe('$21.00');
	});
});
