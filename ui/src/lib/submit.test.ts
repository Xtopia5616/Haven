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
});
