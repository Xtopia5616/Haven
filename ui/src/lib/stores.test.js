import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
	notificationStore,
	addNotification,
	taskMessagesStore,
	setTaskMessages,
	addTaskMessage,
	updateTaskMessages,
	truncateTaskMessages,
	branchTaskMessages,
	adoptDraftMessages,
	clearTaskMessages,
	seqLastSeen,
	pruneSeq,
	clearSeqMap,
	newMessage,
	modelStateStore,
	updateModelState,
	clearModelStateTimer,
} from './stores.js';

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
});

describe('task message store', () => {
	/** @returns {any} */
	const storeMap = () => get(taskMessagesStore);

	beforeEach(() => {
		taskMessagesStore.set({});
	});

	it('setTaskMessages replaces the list for a task', () => {
		const msgs = [{ id: '1' }];
		setTaskMessages('t1', msgs);
		expect(storeMap().t1).toBe(msgs);
	});

	it('addTaskMessage appends and preserves other tasks', () => {
		setTaskMessages('t1', [{ id: '1' }]);
		addTaskMessage('t1', { id: '2' });
		addTaskMessage('t2', { id: 'a' });
		const m = storeMap();
		expect(m.t1.map((x) => x.id)).toEqual(['1', '2']);
		expect(m.t2.map((x) => x.id)).toEqual(['a']);
	});

	it('updateTaskMessages maps existing list', () => {
		setTaskMessages('t1', [{ id: '1', done: false }]);
		updateTaskMessages('t1', (list) => list.map((x) => ({ ...x, done: true })));
		expect(storeMap().t1[0].done).toBe(true);
	});

	it('updateTaskMessages starts from empty list when absent', () => {
		updateTaskMessages('t9', (list) => [...list, { id: 'x' }]);
		expect(storeMap().t9.map((x) => x.id)).toEqual(['x']);
	});

	it('clearTaskMessages removes the task and its seq tracking', () => {
		setTaskMessages('t1', [{ id: '1' }]);
		seqLastSeen('t1-s1', 1);
		clearTaskMessages('t1');
		expect(storeMap().t1).toBeUndefined();
		expect(seqLastSeen('t1-s1', 1)).toBe(false);
	});
});

describe('truncateTaskMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(taskMessagesStore);

	beforeEach(() => {
		taskMessagesStore.set({});
	});

	const msgs = () => [
		{ id: 'a', stepNumber: 1 },
		{ id: 'b', stepNumber: 2 },
		{ id: 'c', role: 'user' },
		{ id: 'd', stepNumber: 3 },
		{ id: 'e', stepNumber: 4 },
	];

	it('drops messages at and after the target step', () => {
		setTaskMessages('t1', msgs());
		truncateTaskMessages('t1', 3);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['a', 'b', 'c']);
	});

	it('keeps everything when no message reaches the target step', () => {
		setTaskMessages('t1', msgs());
		truncateTaskMessages('t1', 99);
		expect(storeMap().t1.map((x) => x.id)).toEqual(['a', 'b', 'c', 'd', 'e']);
	});

	it('is a no-op for a missing task', () => {
		setTaskMessages('t1', msgs());
		truncateTaskMessages('nope', 3);
		expect(storeMap().t1).toHaveLength(5);
	});
});

describe('branchTaskMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(taskMessagesStore);

	beforeEach(() => {
		taskMessagesStore.set({});
	});

	const msgs = () => [
		{ id: 'a', stepNumber: 1 },
		{ id: 'b', stepNumber: 2 },
		{ id: 'c', stepNumber: 3 },
	];

	it('copies messages before the target step into a new task', () => {
		setTaskMessages('src', msgs());
		branchTaskMessages('src', 'dst', 3);
		expect(storeMap().dst.map((x) => x.id)).toEqual(['a', 'b']);
		expect(storeMap().src).toHaveLength(3);
	});

	it('copies the whole list when the target step is never reached', () => {
		setTaskMessages('src', msgs());
		branchTaskMessages('src', 'dst', 99);
		expect(storeMap().dst.map((x) => x.id)).toEqual(['a', 'b', 'c']);
	});
});

describe('adoptDraftMessages', () => {
	/** @returns {any} */
	const storeMap = () => get(taskMessagesStore);

	beforeEach(() => {
		taskMessagesStore.set({});
	});

	it('moves draft messages into the new task after existing ones', () => {
		addTaskMessage('_draft', { id: 'd1' });
		setTaskMessages('t1', [{ id: 'e1' }]);
		adoptDraftMessages('t1');
		const m = storeMap();
		expect(m.t1.map((x) => x.id)).toEqual(['e1', 'd1']);
		expect(m._draft).toEqual([]);
	});

	it('leaves the store untouched when there is no draft', () => {
		setTaskMessages('t1', [{ id: 'e1' }]);
		adoptDraftMessages('t1');
		expect(storeMap().t1).toHaveLength(1);
		expect(storeMap()._draft).toBeUndefined();
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
		expect(seqLastSeen('t-s1', null)).toBe(false);
	});

	it('pruneSeq forgets the step', () => {
		seqLastSeen('t-s1', 3);
		expect(seqLastSeen('t-s1', 3)).toBe(true);
		pruneSeq('t-s1');
		expect(seqLastSeen('t-s1', 3)).toBe(false);
	});

	it('clearSeqMap only removes keys containing the task id', () => {
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
});
