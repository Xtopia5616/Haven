import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { activeTaskIdStore, taskMessagesStore } from './stores.js';

vi.mock('./tauri.js', () => ({
	invoke: vi.fn(),
}));

import { invoke } from './tauri.js';
import { submitTranscript } from './submit.js';

/** @type {import('vitest').Mock} */
const invokeMock = /** @type {any} */ (invoke);

describe('submitTranscript', () => {
	beforeEach(() => {
		invokeMock.mockReset();
		activeTaskIdStore.set(null);
		taskMessagesStore.set({});
	});

	it('appends an optimistic user message under the active task id', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-a');
		await submitTranscript('hello', { voice: false });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'hello',
			activeTaskId: 'task-a',
			attachments: null,
			voice: false,
		});
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-a']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hello');
		expect(list[0].role).toBe('user');
		expect(list[0].voice).toBe(false);
	});

	it('drops the review placeholder when a real message is submitted', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-a');
		// A rolled-back conversation: the DB is empty, so the review rebuild
		// showed a display-only placeholder carrying the task input text.
		taskMessagesStore.set({
			'task-a': [{ id: 'placeholder-task-a', role: 'user', content: '第一条消息' }],
		});
		await submitTranscript('第一条消息', { voice: false });

		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-a']);
		expect(list).toHaveLength(1);
		expect(list[0].id).not.toBe('placeholder-task-a');
		expect(list[0].content).toBe('第一条消息');
	});

	it('keeps the placeholder-less conversation untouched when submitting', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-a');
		taskMessagesStore.set({
			'task-a': [{ id: 'msg-1', role: 'user', content: '第一条消息' }],
		});
		await submitTranscript('第二条消息', { voice: false });

		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-a']);
		expect(list).toHaveLength(2);
		expect(list[0].id).toBe('msg-1');
		expect(list[1].content).toBe('第二条消息');
	});

	it('appends under _draft when there is no active task', async () => {
		invokeMock.mockResolvedValue({});
		await submitTranscript('orphan', { voice: true });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'orphan',
			activeTaskId: null,
			attachments: null,
			voice: true,
		});
		const draft = /** @type {any[]} */ (get(taskMessagesStore)['_draft']);
		expect(draft).toHaveLength(1);
		expect(draft[0].voice).toBe(true);
	});

	it('passes images through to process_transcript and tags the optimistic bubble', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-img');
		const images = [{ media_type: 'image/png', data: 'abc' }];
		await submitTranscript('see pic', { images });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'see pic',
			activeTaskId: 'task-img',
			attachments: images,
			voice: false,
		});
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-img']);
		expect(list[0].attachments).toEqual(images);
		expect(list[0].id).toMatch(/-u-[a-z0-9]+$/);
	});

	it('combines images and files into one attachments payload', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-mix');
		const images = [{ media_type: 'image/png', data: 'abc' }];
		const files = [{ media_type: 'application/pdf', data: 'cGVvcGxl', filename: 'doc.pdf' }];
		await submitTranscript('read these', { images, files });

		expect(invoke).toHaveBeenCalledWith('process_transcript', {
			transcript: 'read these',
			activeTaskId: 'task-mix',
			attachments: [...images, ...files],
			voice: false,
		});
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-mix']);
		expect(list[0].attachments).toEqual([...images, ...files]);
	});

	it('coerces an empty image array to null on the wire', async () => {
		invokeMock.mockResolvedValue({});
		activeTaskIdStore.set('task-empty');
		await submitTranscript('text only', { images: [] });

		expect(invoke).toHaveBeenCalledWith(
			'process_transcript',
			expect.objectContaining({ attachments: null })
		);
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-empty']);
		expect(list[0].attachments).toEqual([]);
	});

	it('migrates the optimistic bubble into the newly created task', async () => {
		invokeMock.mockResolvedValue({ TaskCreated: 'task-new' });
		await submitTranscript('hi', { voice: false });

		expect(get(activeTaskIdStore)).toBe('task-new');
		expect(get(taskMessagesStore)['_draft']).toEqual([]);
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-new']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
	});

	it('migrates from a stale task id when TaskCreated differs', async () => {
		invokeMock.mockResolvedValue({ TaskCreated: 'task-new' });
		activeTaskIdStore.set('task-stale');
		await submitTranscript('hi', { voice: false });

		expect(get(activeTaskIdStore)).toBe('task-new');
		expect(get(taskMessagesStore)['task-stale']).toEqual([]);
		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-new']);
		expect(list).toHaveLength(1);
	});

	it('does not move messages when TaskCreated equals the current key', async () => {
		invokeMock.mockResolvedValue({ TaskCreated: 'task-same' });
		activeTaskIdStore.set('task-same');
		await submitTranscript('hi', { voice: false });

		const list = /** @type {any[]} */ (get(taskMessagesStore)['task-same']);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
	});

	it('removes the optimistic bubble and rethrows when invoke rejects', async () => {
		invokeMock.mockRejectedValue(new Error('boom'));
		activeTaskIdStore.set('task-fail');
		await expect(submitTranscript('oops')).rejects.toThrow('boom');

		expect(get(taskMessagesStore)['task-fail']).toEqual([]);
	});

	it('uses the active task id at submission time, not the eventual TaskCreated', async () => {
		invokeMock.mockResolvedValue({ TaskCreated: 'task-new' });
		activeTaskIdStore.set('task-a');
		await submitTranscript('hi');
		expect(/** @type {any[]} */ (invokeMock.mock.calls)[0][1].activeTaskId).toBe('task-a');
	});
});
