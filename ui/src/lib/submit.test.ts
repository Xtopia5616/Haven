import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { activeSessionIdStore, sessionMessagesStore } from './stores.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

import { invoke } from './tauri.ts';
import { submitTranscript } from './submit.ts';

const invokeMock = invoke as any;

describe('submitTranscript', () => {
	beforeEach(() => {
		invokeMock.mockReset();
		activeSessionIdStore.set(null);
		sessionMessagesStore.set({});
	});

	it('appends an optimistic user message under the active session id', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-a');
		await submitTranscript('hello', { voice: false });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'hello',
			activeSessionId: 'session-a',
			attachments: null,
			voice: false,
		});
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hello');
		expect(list[0].role).toBe('user');
		expect(list[0].voice).toBe(false);
	});

	it('drops the review placeholder when a real message is submitted', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-a');
		// A rolled-back conversation: the DB is empty, so the review rebuild
		// showed a display-only placeholder carrying the session input text.
		sessionMessagesStore.set({
			'session-a': [{ id: 'placeholder-session-a', role: 'user', content: '第一条消息' }],
		});
		await submitTranscript('第一条消息', { voice: false });

		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list).toHaveLength(1);
		expect(list[0].id).not.toBe('placeholder-session-a');
		expect(list[0].content).toBe('第一条消息');
	});

	it('keeps the placeholder-less conversation untouched when submitting', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-a');
		sessionMessagesStore.set({
			'session-a': [{ id: 'msg-1', role: 'user', content: '第一条消息' }],
		});
		await submitTranscript('第二条消息', { voice: false });

		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list).toHaveLength(2);
		expect(list[0].id).toBe('msg-1');
		expect(list[1].content).toBe('第二条消息');
	});

	it('appends under _draft when there is no active session', async () => {
		invokeMock.mockResolvedValue({});
		await submitTranscript('orphan', { voice: true });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'orphan',
			activeSessionId: null,
			attachments: null,
			voice: true,
		});
		const draft = /** @type {any[]} */ (get(sessionMessagesStore)['_draft']);
		expect(draft).toHaveLength(1);
		expect(draft[0].voice).toBe(true);
	});

	it('passes images through to process_transcript and tags the optimistic bubble', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-img');
		const images = [{ media_type: 'image/png', data: 'abc' }];
		await submitTranscript('see pic', { images });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'see pic',
			activeSessionId: 'session-img',
			attachments: images,
			voice: false,
		});
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-img']);
		expect(list[0].attachments).toEqual(images);
		expect(list[0].id).toMatch(/-u-[a-z0-9]+$/);
	});

	it('combines images and files into one attachments payload', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-mix');
		const images = [{ media_type: 'image/png', data: 'abc' }];
		const files = [{ media_type: 'application/pdf', data: 'cGVvcGxl', filename: 'doc.pdf' }];
		await submitTranscript('read these', { images, files });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'read these',
			activeSessionId: 'session-mix',
			attachments: [...images, ...files],
			voice: false,
		});
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-mix']);
		expect(list[0].attachments).toEqual([...images, ...files]);
	});

	it('coerces an empty image array to null on the wire', async () => {
		invokeMock.mockResolvedValue({});
		activeSessionIdStore.set('session-empty');
		await submitTranscript('text only', { images: [] });

		expect(invoke).toHaveBeenCalledWith(
			'process_transcript',
			expect.objectContaining({ attachments: null })
		);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-empty']);
		expect(list[0].attachments).toEqual([]);
	});

	it('migrates the optimistic bubble into the newly created session', async () => {
		invokeMock.mockResolvedValue({ SessionCreated: 'session-new' });
		await submitTranscript('hi', { voice: false });

		expect(get(activeSessionIdStore)).toBe('session-new');
		expect(get(sessionMessagesStore)['_draft']).toEqual([]);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-new']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
	});

	it('migrates from a stale session id when SessionCreated differs', async () => {
		invokeMock.mockResolvedValue({ SessionCreated: 'session-new' });
		activeSessionIdStore.set('session-stale');
		await submitTranscript('hi', { voice: false });

		expect(get(activeSessionIdStore)).toBe('session-new');
		expect(get(sessionMessagesStore)['session-stale']).toEqual([]);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-new']);
		expect(list).toHaveLength(1);
	});

	it('does not move messages when SessionCreated equals the current key', async () => {
		invokeMock.mockResolvedValue({ SessionCreated: 'session-same' });
		activeSessionIdStore.set('session-same');
		await submitTranscript('hi', { voice: false });

		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-same']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
	});

	it('removes the optimistic bubble and rethrows when invoke rejects', async () => {
		invokeMock.mockRejectedValue(new Error('boom'));
		activeSessionIdStore.set('session-fail');
		await expect(submitTranscript('oops')).rejects.toThrow('boom');

		expect(get(sessionMessagesStore)['session-fail']).toEqual([]);
	});

	it('uses the active session id at submission time, not the eventual SessionCreated', async () => {
		invokeMock.mockResolvedValue({ SessionCreated: 'session-new' });
		activeSessionIdStore.set('session-a');
		await submitTranscript('hi');
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[0][1].activeSessionId).toBe('session-a');
	});

	it('joins an in-flight submission instead of stacking duplicates', async () => {
		let resolveInvoke: (v: unknown) => void;
		invokeMock.mockReturnValue(
			new Promise((resolve) => {
				resolveInvoke = resolve;
			})
		);
		activeSessionIdStore.set('session-a');

		const first = submitTranscript('继续', { voice: false });
		const second = submitTranscript('继续', { voice: false });
		// Both callers share ONE in-flight promise: the invoke is called once.
		expect(invokeMock).toHaveBeenCalledTimes(1);

		resolveInvoke!({});
		await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(1);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list).toHaveLength(1); // a concurrent duplicate must not add a second bubble
	});

	it('does not join the same text when the session pin differs', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce({});
		activeSessionIdStore.set('session-a');
		const first = submitTranscript('继续', { voice: false });
		activeSessionIdStore.set('session-b');
		const second = submitTranscript('继续', { voice: false });
		expect(invokeMock).toHaveBeenCalledTimes(1);
		resolveFirst!({});
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[1][1].activeSessionId).toBe(
			'session-b',
		);
	});

	it('adopts the newly created session for a queued draft follow-up', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce({});
		activeSessionIdStore.set(null);
		const first = submitTranscript('第一条', { voice: false });
		const second = submitTranscript('第二条', { voice: true });
		expect(invokeMock).toHaveBeenCalledTimes(1);
		resolveFirst!({ SessionCreated: 'session-new' });
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[1][1].activeSessionId).toBe(
			'session-new',
		);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-new']);
		expect(list.map((x) => x.content)).toEqual(['第一条', '第二条']);
	});

	it('adopts after a fresh-start create even when both were queued with intent', async () => {
		const { newSessionIntentStore } = await import('./stores.ts');
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce({});
		activeSessionIdStore.set(null);
		newSessionIntentStore.set(true);
		const first = submitTranscript('新对话第一条', { voice: false });
		const second = submitTranscript('新对话第二条', { voice: true });
		expect(invokeMock).toHaveBeenCalledTimes(1);
		resolveFirst!({ SessionCreated: 'session-fresh' });
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[1][1].activeSessionId).toBe(
			'session-fresh',
		);
		newSessionIntentStore.set(false);
	});

	it('queues a DIFFERENT concurrent submission instead of dropping it', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				})
			)
			.mockResolvedValueOnce({});
		activeSessionIdStore.set('session-a');

		const first = submitTranscript('第一条', { voice: false });
		const second = submitTranscript('第二条', { voice: false });
		// Only the first dispatched so far; the second is queued, not dropped.
		expect(invokeMock).toHaveBeenCalledTimes(1);

		resolveFirst!({});
		const results = await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(results[1]).toEqual({});
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list.map((x) => x.content)).toEqual(['第一条', '第二条']);
	});

	it('keeps the enqueue-time session for a queued submission after a switch', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				})
			)
			.mockResolvedValueOnce({});
		activeSessionIdStore.set('session-a');

		const first = submitTranscript('给 A', { voice: false });
		const second = submitTranscript('也给 A', { voice: false });
		// Switch away while the second is still queued.
		activeSessionIdStore.set('session-b');
		resolveFirst!({});
		await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[1][1].activeSessionId).toBe(
			'session-a',
		);
		const listA = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		const listB = /** @type {any[]} */ (get(sessionMessagesStore)['session-b']);
		expect(listA.map((x) => x.content)).toEqual(['给 A', '也给 A']);
		expect(listB || []).toEqual([]);
	});

	it('releases the lock after a failed submission so a retry can submit again', async () => {
		invokeMock.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce({});
		activeSessionIdStore.set('session-a');
		await expect(submitTranscript('try 1')).rejects.toThrow('boom');
		await submitTranscript('try 2');

		expect(invokeMock).toHaveBeenCalledTimes(2);
		const list = /** @type {any[]} */ (get(sessionMessagesStore)['session-a']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('try 2');
	});
});
