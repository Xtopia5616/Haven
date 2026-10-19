import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { reactExecutionPhaseStore } from './reactExecutionPhaseStore.ts';
import { newSessionIntentStore } from './sessionIntentStore.ts';
import { SessionReducer } from './sessionReducer.ts';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn(),
}));

import { invoke } from './tauri.ts';
import { submitTranscript as submitWithReducer } from './submit.ts';

const SESSION_A = 'ses-efe910e5aabbdf9a4747892b4359818f';
const SESSION_B = 'ses-851143562c1031fee321946d737a0e15';
const SESSION_IMG = 'ses-fd7557c1b1b6e80f53d0bd81c8b41723';
const SESSION_MIX = 'ses-3f2cdef8fd463c21da5263cbed357ec7';
const SESSION_EMPTY = 'ses-ef40375cdc9832aa3194cde8ebdf843f';
const SESSION_STALE = 'ses-fd7266ec1d3b4717e098c8cdc0d8a9ee';
const SESSION_FAIL = 'ses-0000008c3defb1edb984fe2ac71c71c7';
const SESSION_DRAFT = 'ses-000001187bdf63db7309fc558e38e38e';

function supplemented(sessionId: string, messageId?: string) {
	return {
		Supplemented: { session_id: sessionId, ...(messageId ? { message_id: messageId } : {}) },
	};
}

const invokeMock = invoke as any;
let reducer: SessionReducer;

const messagesFor = (sessionId: string) => reducer.getMessages(sessionId);
const select = (sessionId: string | null) =>
	reducer.dispatch({ type: 'session/selected', sessionId });
const loadMessages = (sessionId: string, messages: any[]) =>
	reducer.dispatch({ type: 'session/messages/resume-loaded', sessionId, messages });

function submitTranscript(text: string, options: Record<string, unknown> = {}) {
	return submitWithReducer(text, { ...options, reducer } as any);
}

describe('submitTranscript', () => {
	beforeEach(() => {
		invokeMock.mockReset();
		reducer = new SessionReducer();
		newSessionIntentStore.set(false);
		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
	});

	it('preserves explicit follow-up mode through the submission lane', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_A));
		select(SESSION_A);
		await submitTranscript('afterwards', { deliveryMode: 'follow_up' });
		expect(invoke).toHaveBeenCalledWith(
			'submit_transcript',
			expect.objectContaining({ deliveryMode: 'follow_up' }),
		);
	});

	it('appends an optimistic user message under the active session id', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_A));
		select(SESSION_A);
		await submitTranscript('hello', { voice: false });

		expect(invoke).toHaveBeenCalledWith('submit_transcript', {
			transcript: 'hello',
			activeSessionId: SESSION_A,
			attachments: null,
			voice: false,
		});
		const list = messagesFor(SESSION_A);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hello');
		expect(list[0].role).toBe('user');
		expect(list[0].voice).toBe(false);
	});

	it('preserves the persisted transcript when submitting', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_A));
		select(SESSION_A);
		loadMessages(SESSION_A, [
			{
				id: 'msg-00000000000000000000000000000014',
				role: 'user' as const,
				content: '第一条消息',
			},
		]);
		await submitTranscript('第二条消息', { voice: false });

		const list = messagesFor(SESSION_A);
		expect(list).toHaveLength(2);
		expect(list[0].id).toBe('msg-00000000000000000000000000000014');
		expect(list[1].content).toBe('第二条消息');
	});

	it('moves a draft input to the session created by ingress', async () => {
		let resolveInvoke!: (value: unknown) => void;
		invokeMock.mockReturnValue(
			new Promise((resolve) => {
				resolveInvoke = resolve;
			}),
		);
		const pending = submitTranscript('orphan', { voice: true });

		expect(invoke).toHaveBeenCalledWith('submit_transcript', {
			transcript: 'orphan',
			activeSessionId: null,
			attachments: null,
			voice: true,
		});
		expect(messagesFor('_draft')).toHaveLength(1);
		resolveInvoke!({
			SessionCreated: {
				session_id: SESSION_DRAFT,
				message_id: 'msg-000001187bdf63db7309fc558e38e38e',
			},
		});
		await pending;
		expect(messagesFor('_draft')).toEqual([]);
		expect(messagesFor(SESSION_DRAFT)[0].voice).toBe(true);
	});

	it('keeps a newer user selection when an older submit creates a session', async () => {
		let resolveInvoke!: (value: unknown) => void;
		invokeMock.mockReturnValue(
			new Promise((resolve) => {
				resolveInvoke = resolve;
			}),
		);
		select(SESSION_A);
		const pending = submitTranscript('延迟中的发送', { voice: true });

		reducer.beginUserSelection();
		select(SESSION_B);
		resolveInvoke!({
			SessionCreated: {
				session_id: SESSION_DRAFT,
				message_id: 'msg-000001187bdf63db7309fc558e38e38e',
			},
		});
		await pending;

		expect(reducer.snapshot().activeSessionId).toBe(SESSION_B);
		expect(messagesFor(SESSION_DRAFT)).toHaveLength(1);
		expect(messagesFor(SESSION_DRAFT)[0].content).toBe('延迟中的发送');
	});

	it('passes the recording id so ingress STT usage can bind to the session', async () => {
		invokeMock.mockResolvedValue({
			SessionCreated: {
				session_id: SESSION_DRAFT,
				message_id: 'msg-000001a4b9cf15c92c8efa8055555555',
			},
		});
		await submitTranscript('voice text', {
			voice: true,
			recordingId: 'rec-00000000000000000000000000000000',
		});

		expect(invoke).toHaveBeenCalledWith('submit_transcript', {
			transcript: 'voice text',
			activeSessionId: null,
			attachments: null,
			voice: true,
			recordingId: 'rec-00000000000000000000000000000000',
		});
	});

	it('passes images through to submit_transcript and tags the optimistic bubble', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_IMG));
		select(SESSION_IMG);
		const images = [{ media_type: 'image/png', data: 'abc' }];
		await submitTranscript('see pic', { images });

		expect(invoke).toHaveBeenCalledWith('submit_transcript', {
			transcript: 'see pic',
			activeSessionId: SESSION_IMG,
			attachments: images,
			voice: false,
		});
		const list = messagesFor(SESSION_IMG);
		expect(list[0].attachments).toEqual(images);
		expect(list[0].id).toMatch(/-u-[a-z0-9]+$/);
	});

	it('combines images and files into one attachments payload', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_MIX));
		select(SESSION_MIX);
		const images = [{ media_type: 'image/png', data: 'abc' }];
		const files = [{ media_type: 'application/pdf', data: 'cGVvcGxl', filename: 'doc.pdf' }];
		await submitTranscript('read these', { images, files });

		expect(invoke).toHaveBeenCalledWith('submit_transcript', {
			transcript: 'read these',
			activeSessionId: SESSION_MIX,
			attachments: [...images, ...files],
			voice: false,
		});
		const list = messagesFor(SESSION_MIX);
		expect(list[0].attachments).toEqual([...images, ...files]);
	});

	it('coerces an empty image array to null on the wire', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_EMPTY));
		select(SESSION_EMPTY);
		await submitTranscript('text only', { images: [] });

		expect(invoke).toHaveBeenCalledWith(
			'submit_transcript',
			expect.objectContaining({ attachments: null }),
		);
		const list = messagesFor(SESSION_EMPTY);
		expect(list[0].attachments).toEqual([]);
	});

	it('migrates the optimistic bubble into the newly created session', async () => {
		invokeMock.mockResolvedValue({
			SessionCreated: {
				session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
				message_id: 'msg-00000000000000000000000000000001',
			},
		});
		await submitTranscript('hi', { voice: false });

		expect(reducer.snapshot().activeSessionId).toBe('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(messagesFor('_draft')).toEqual([]);
		const list = messagesFor('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
		expect(list[0].id).toBe('msg-00000000000000000000000000000001');
	});

	it('migrates from a stale session id when SessionCreated differs', async () => {
		invokeMock.mockResolvedValue({
			SessionCreated: {
				session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
				message_id: 'msg-00000000000000000000000000000002',
			},
		});
		select(SESSION_STALE);
		await submitTranscript('hi', { voice: false });

		expect(reducer.snapshot().activeSessionId).toBe('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(messagesFor(SESSION_STALE)).toEqual([]);
		const list = messagesFor('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(list).toHaveLength(1);
		expect(list[0].id).toBe('msg-00000000000000000000000000000002');
	});

	it('does not move messages when SessionCreated equals the current key', async () => {
		invokeMock.mockResolvedValue({
			SessionCreated: {
				session_id: 'ses-000001187bdf63db7309fc558e38e38e',
				message_id: 'msg-00000000000000000000000000000003',
			},
		});
		select('ses-000001187bdf63db7309fc558e38e38e');
		await submitTranscript('hi', { voice: false });

		const list = messagesFor('ses-000001187bdf63db7309fc558e38e38e');
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('hi');
		expect(list[0].id).toBe('msg-00000000000000000000000000000003');
	});

	it('rewrites the optimistic bubble id from Supplemented.message_id', async () => {
		invokeMock.mockResolvedValue({
			Supplemented: {
				session_id: SESSION_A,
				message_id: 'msg-00000000000000000000000000000004',
			},
		});
		select(SESSION_A);
		await submitTranscript('hello', { voice: false });
		const list = messagesFor(SESSION_A);
		expect(list).toHaveLength(1);
		expect(list[0].id).toBe('msg-00000000000000000000000000000004');
	});

	it('marks a mid-turn send as steering so agent UI stays above it', async () => {
		invokeMock.mockResolvedValue({
			Supplemented: {
				session_id: SESSION_A,
				message_id: 'msg-00000000000000000000000000000005',
			},
		});
		select(SESSION_A);
		loadMessages(SESSION_A, [
			{
				id: 'msg-00000000000000000000000000000014',
				role: 'user' as const,
				content: 'hi',
				received: true,
			},
			{
				id: 'msg-00000000000000000000000000000015',
				role: 'assistant' as const,
				content: '想',
				streaming: true,
			},
		]);
		reactExecutionPhaseStore.set({ sessionId: SESSION_A, phase: 'generating' });
		await submitTranscript('补充', { voice: false });
		const list = messagesFor(SESSION_A);
		expect(list).toHaveLength(3);
		expect(list[2]).toMatchObject({
			id: 'msg-00000000000000000000000000000005',
			content: '补充',
			steering: true,
		});
	});

	it('does not mark an idle new-turn send as steering', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_A));
		select(SESSION_A);
		loadMessages(SESSION_A, [
			{
				id: 'msg-00000000000000000000000000000014',
				role: 'user' as const,
				content: 'hi',
				received: true,
			},
			{
				id: 'msg-00000000000000000000000000000015',
				role: 'assistant' as const,
				content: '好的',
				streaming: false,
			},
		]);
		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
		await submitTranscript('下一题', { voice: false });
		const list = messagesFor(SESSION_A);
		expect(list[2].content).toBe('下一题');
		expect(list[2].steering).toBeFalsy();
	});

	it('marks a follow-up as steering when the prior user is still awaiting agent UI', async () => {
		invokeMock.mockResolvedValue({
			Supplemented: {
				session_id: SESSION_A,
				message_id: 'msg-00000000000000000000000000000006',
			},
		});
		select(SESSION_A);
		// First message accepted, execution phase not flipped yet, no assistant bubble.
		loadMessages(SESSION_A, [
			{ id: 'msg-00000000000000000000000000000014', role: 'user' as const, content: 'hi' },
		]);
		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
		await submitTranscript('再加一句', { voice: false });
		const list = messagesFor(SESSION_A);
		expect(list[1]).toMatchObject({
			id: 'msg-00000000000000000000000000000006',
			steering: true,
		});
	});

	it('does not steal steering from a parallel busy session via the global execution phase', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_B));
		// Active session B is idle; the global execution phase reflects busy session A.
		select(SESSION_B);
		loadMessages(SESSION_B, [
			{
				id: 'msg-00000000000000000000000000000016',
				role: 'user' as const,
				content: 'hi',
				received: true,
			},
			{
				id: 'msg-00000000000000000000000000000017',
				role: 'assistant' as const,
				content: '好的',
				streaming: false,
			},
		]);
		reactExecutionPhaseStore.set({ sessionId: SESSION_A, phase: 'generating' });
		await submitTranscript('下一题', { voice: false });
		const list = messagesFor(SESSION_B);
		expect(list[2].content).toBe('下一题');
		expect(list[2].steering).toBeFalsy();
	});

	it('does not treat another session phase as steering for an empty active transcript', async () => {
		invokeMock.mockResolvedValue(supplemented(SESSION_B));
		select(SESSION_B);
		// A background session may update the shell's latest phase while the
		// selected session has not loaded or produced any transcript yet.
		reactExecutionPhaseStore.set({ sessionId: SESSION_A, phase: 'generating' });

		await submitTranscript('first message', { voice: false });

		expect(messagesFor(SESSION_B)).toHaveLength(1);
		expect(messagesFor(SESSION_B)[0].steering).toBeFalsy();
	});

	it('removes the optimistic bubble and rethrows when invoke rejects', async () => {
		invokeMock.mockRejectedValue(new Error('boom'));
		select(SESSION_FAIL);
		await expect(submitTranscript('oops')).rejects.toThrow('boom');

		expect(messagesFor(SESSION_FAIL)).toEqual([]);
	});

	it('uses the active session id at submission time, not the eventual SessionCreated', async () => {
		invokeMock.mockResolvedValue({
			SessionCreated: {
				session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
				message_id: 'msg-00000000000000000000000000000007',
			},
		});
		select(SESSION_A);
		await submitTranscript('hi');
		expect(/** @type {any[]} */ invokeMock.mock.calls[0][1].activeSessionId).toBe(SESSION_A);
	});

	it('joins an in-flight submission only when callers share its request token', async () => {
		let resolveInvoke: (v: unknown) => void;
		invokeMock.mockReturnValue(
			new Promise((resolve) => {
				resolveInvoke = resolve;
			}),
		);
		select(SESSION_A);

		const first = submitTranscript('继续', {
			voice: false,
			submissionToken: 'continue:msg-00000000000000000000000000000020',
		});
		const second = submitTranscript('继续', {
			voice: false,
			submissionToken: 'continue:msg-00000000000000000000000000000020',
		});
		// Both callers share ONE in-flight promise: the invoke is called once.
		expect(invokeMock).toHaveBeenCalledTimes(1);

		resolveInvoke!(supplemented(SESSION_A));
		await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(1);
		const list = messagesFor(SESSION_A);
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
			.mockResolvedValueOnce(supplemented(SESSION_B));
		select(SESSION_A);
		const first = submitTranscript('继续', { voice: false });
		select(SESSION_B);
		const second = submitTranscript('继续', { voice: false });
		// Different session lanes are independent; switching sessions must not
		// make B wait for an unrelated in-flight request in A.
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ invokeMock.mock.calls[1][1].activeSessionId).toBe(SESSION_B);
		resolveFirst!(supplemented(SESSION_A));
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
	});

	it('adopts the newly created session for a queued draft follow-up', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce(supplemented('ses-0000008c3defb1edb984fe2ac71c71c7'));
		select(null);
		const first = submitTranscript('第一条', { voice: false });
		const second = submitTranscript('第二条', { voice: true });
		expect(invokeMock).toHaveBeenCalledTimes(1);
		resolveFirst!({
			SessionCreated: {
				session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
				message_id: 'msg-00000000000000000000000000000008',
			},
		});
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ invokeMock.mock.calls[1][1].activeSessionId).toBe(
			'ses-0000008c3defb1edb984fe2ac71c71c7',
		);
		const list = messagesFor('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(list.map((x) => x.content)).toEqual(['第一条', '第二条']);
	});

	it('adopts after a fresh-start create even when both were queued with intent', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce(supplemented('ses-000001a4b9cf15c92c8efa8055555555'));
		select(null);
		newSessionIntentStore.set(true);
		const first = submitTranscript('新对话第一条', { voice: false });
		const second = submitTranscript('新对话第二条', { voice: true });
		expect(invokeMock).toHaveBeenCalledTimes(1);
		resolveFirst!({
			SessionCreated: {
				session_id: 'ses-000001a4b9cf15c92c8efa8055555555',
				message_id: 'msg-00000000000000000000000000000009',
			},
		});
		await Promise.all([first, second]);
		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ invokeMock.mock.calls[1][1].activeSessionId).toBe(
			'ses-000001a4b9cf15c92c8efa8055555555',
		);
		newSessionIntentStore.set(false);
	});

	it('queues a same-content submission without a shared request token', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce(supplemented(SESSION_A));
		select(SESSION_A);

		const first = submitTranscript('相同内容', { voice: false });
		const second = submitTranscript('相同内容', { voice: false });
		// Only the first dispatched so far; the second is queued, not dropped.
		expect(invokeMock).toHaveBeenCalledTimes(1);

		resolveFirst!(supplemented(SESSION_A));
		const results = await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(results[1]).toEqual({ kind: 'supplemented', sessionId: SESSION_A });
		const list = messagesFor(SESSION_A);
		expect(list.map((x) => x.content)).toEqual(['相同内容', '相同内容']);
	});

	it('does not let a newly created session overtake queued draft sends', async () => {
		let resolveFirst!: (v: unknown) => void;
		let resolveSecond!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveSecond = resolve;
				}),
			)
			.mockResolvedValueOnce(supplemented('ses-0000008c3defb1edb984fe2ac71c71c7'));
		select(null);

		const first = submitTranscript('创建会话', { voice: false });
		const second = submitTranscript('排队消息', { voice: false });
		resolveFirst!({
			SessionCreated: {
				session_id: 'ses-0000008c3defb1edb984fe2ac71c71c7',
				message_id: 'msg-0000000000000000000000000000000a',
			},
		});
		// The queued message owns the new session lane now. A third send to the
		// active session must queue behind it instead of overtaking it.
		await vi.waitFor(() =>
			expect(reducer.snapshot().activeSessionId).toBe('ses-0000008c3defb1edb984fe2ac71c71c7'),
		);
		const third = submitTranscript('不能超车', { voice: false });
		expect(invokeMock).toHaveBeenCalledTimes(2);

		await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));
		expect(/** @type {any[]} */ invokeMock.mock.calls[1][1].activeSessionId).toBe(
			'ses-0000008c3defb1edb984fe2ac71c71c7',
		);
		resolveSecond!(supplemented('ses-0000008c3defb1edb984fe2ac71c71c7'));
		await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(3));
		await Promise.all([first, second, third]);
		expect(invokeMock).toHaveBeenCalledTimes(3);
		expect(/** @type {any[]} */ invokeMock.mock.calls[2][1].activeSessionId).toBe(
			'ses-0000008c3defb1edb984fe2ac71c71c7',
		);
		const list = messagesFor('ses-0000008c3defb1edb984fe2ac71c71c7');
		expect(list.map((x) => x.content)).toEqual(['创建会话', '排队消息', '不能超车']);
	});

	it('keeps the enqueue-time session for a queued submission after a switch', async () => {
		let resolveFirst!: (v: unknown) => void;
		invokeMock
			.mockReturnValueOnce(
				new Promise((resolve) => {
					resolveFirst = resolve;
				}),
			)
			.mockResolvedValueOnce(supplemented(SESSION_A));
		select(SESSION_A);

		const first = submitTranscript('给 A', { voice: false });
		const second = submitTranscript('也给 A', { voice: false });
		// Switch away while the second is still queued.
		select(SESSION_B);
		resolveFirst!(supplemented(SESSION_A));
		await Promise.all([first, second]);

		expect(invokeMock).toHaveBeenCalledTimes(2);
		expect(/** @type {any[]} */ invokeMock.mock.calls[1][1].activeSessionId).toBe(SESSION_A);
		const listA = messagesFor(SESSION_A);
		const listB = messagesFor(SESSION_B);
		expect(listA.map((x) => x.content)).toEqual(['给 A', '也给 A']);
		expect(listB || []).toEqual([]);
	});

	it('releases the lock after a failed submission so a retry can submit again', async () => {
		invokeMock
			.mockRejectedValueOnce(new Error('boom'))
			.mockResolvedValueOnce(supplemented(SESSION_A));
		select(SESSION_A);
		await expect(submitTranscript('try 1')).rejects.toThrow('boom');
		await submitTranscript('try 2');

		expect(invokeMock).toHaveBeenCalledTimes(2);
		const list = messagesFor(SESSION_A);
		expect(list).toHaveLength(1);
		expect(list[0].content).toBe('try 2');
	});

	it('rejects malformed command identities and removes the optimistic message', async () => {
		invokeMock.mockResolvedValue({ SessionCreated: { session_id: 'ses-short' } });

		await expect(submitTranscript('bad response')).rejects.toThrow(
			'Invalid transcript submission identity',
		);
		expect(messagesFor('_draft')).toEqual([]);
	});
});
