import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ChatAttachmentPayload, ChatFileAttachment } from './chatAttachmentTypes.ts';
import { createAskInteractionController } from './chatAskInteraction.ts';
import { SessionReducer } from './sessionReducer.ts';

const SESSION_ID = 'ses-ask';

function createRequest(
	id: string,
	question: string,
	messageOptions: string[] = [],
	requestOptions = messageOptions,
) {
	return {
		question,
		messageOptions,
		request: {
			id,
			sessionId: SESSION_ID,
			owner: { kind: 'session' as const, sessionId: SESSION_ID },
			kind: 'ask' as const,
			status: 'pending' as const,
			options: requestOptions,
			createdAt: '',
		},
	};
}

describe('createAskInteractionController', () => {
	let reducer: SessionReducer;

	beforeEach(() => {
		reducer = new SessionReducer();
	});

	function createController(submitMessage = vi.fn()) {
		const ready = vi.fn();
		const setAutoFollow = vi.fn();
		const controller = createAskInteractionController({
			getActiveSessionId: () => SESSION_ID,
			setAutoFollow,
			setSelectionsReady: ready,
			submitMessage,
			reducer,
		});
		return { controller, ready, submitMessage, setAutoFollow };
	}

	function loadAskMessages(...requests: ReturnType<typeof createRequest>[]) {
		reducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: SESSION_ID,
			messages: requests.map(({ question, messageOptions, request }) => ({
				id: request.id,
				type: 'ask',
				content: question,
				options: messageOptions,
				awaiting: true,
			})),
			interactions: requests.map(({ request }) => request),
		});
	}

	it('requires every awaiting ask to be selected before batch submit', () => {
		loadAskMessages(
			createRequest('ask-1', '第一个问题', ['A']),
			createRequest('ask-2', '第二个问题', ['B']),
		);
		const { controller, ready, submitMessage } = createController();

		controller.handleAskSelectionChange('ask-1', ['A']);
		expect(ready).toHaveBeenLastCalledWith(false);
		expect(controller.trySubmitAskSelections(SESSION_ID, '', [], [])).toBe(false);

		controller.handleAskSelectionChange('ask-2', ['B']);
		expect(ready).toHaveBeenLastCalledWith(true);
		expect(controller.trySubmitAskSelections(SESSION_ID, '补充', [], [])).toBe(true);
		expect(submitMessage).toHaveBeenCalledOnce();
		expect(submitMessage).toHaveBeenCalledWith(
			'关于「第一个问题」：A\n关于「第二个问题」：B 补充',
			[],
			[],
		);
		expect(reducer.snapshot().interactions).toMatchObject({
			'ask-1': expect.objectContaining({ status: 'resolved', response: { answer: 'A' } }),
			'ask-2': expect.objectContaining({ status: 'resolved', response: { answer: 'B' } }),
		});
	});

	it('keeps selected options available when a pending question is reopened', () => {
		loadAskMessages(createRequest('ask-restored', '选择一个选项', ['A', 'B']));
		const { controller } = createController();

		controller.handleAskSelectionChange('ask-restored', ['B']);

		expect(controller.getAskSelection('ask-restored')).toEqual(['B']);
	});

	it('submits an ignored ask once and rejects duplicate resolution', () => {
		loadAskMessages(createRequest('ask-1', '要继续吗？'));
		const { controller, submitMessage } = createController();

		controller.handleIgnoreAsk('ask-1');
		controller.handleIgnoreAsk('ask-1');

		expect(submitMessage).toHaveBeenCalledOnce();
		expect(submitMessage).toHaveBeenCalledWith('忽略', [], []);
	});

	it('reads a resolved answer from the reducer when submitting the batch', () => {
		loadAskMessages(
			createRequest('ask-1', '第一个问题'),
			createRequest('ask-2', '第二个问题'),
		);
		const { controller, submitMessage } = createController();

		controller.handleIgnoreAsk('ask-1');
		const firstResolved = reducer.snapshot().interactions['ask-1'];
		if (!firstResolved || firstResolved.kind !== 'ask')
			throw new Error('expected an Ask interaction to be resolved');
		reducer.dispatch({
			type: 'session/interaction-upserted',
			request: { ...firstResolved, response: { answer: 'reducer answer' } },
		});
		controller.handleIgnoreAsk('ask-2');

		expect(submitMessage).toHaveBeenCalledWith(
			'关于「第一个问题」：reducer answer\n关于「第二个问题」：忽略',
			[],
			[],
		);
	});

	it('submits only answers resolved in the current controller batch', () => {
		loadAskMessages(createRequest('ask-history', '历史问题'));
		const first = createController();
		first.controller.handleIgnoreAsk('ask-history');

		const oldResolved = reducer.snapshot().interactions['ask-history'];
		const current = createRequest('ask-current', '当前问题', ['选项']);
		reducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: SESSION_ID,
			messages: [
				{ id: 'ask-history', type: 'ask', content: '历史问题', awaiting: false },
				{ id: current.request.id, type: 'ask', content: current.question, awaiting: true },
			],
			interactions: [oldResolved!, current.request],
		});
		const second = createController();
		second.controller.handleAskSelectionChange('ask-current', ['选项']);
		second.controller.handleAskSubmit();

		expect(second.submitMessage).toHaveBeenCalledOnce();
		expect(second.submitMessage).toHaveBeenCalledWith('选项', [], []);
	});

	it('routes composer input through a fully selected ask batch and appends typed text', () => {
		loadAskMessages(
			// Quick choices come from the live tool observation; the interaction
			// event that gates the ask carries no option list.
			createRequest('ask-1', '第一个问题', ['A'], []),
			createRequest('ask-2', '第二个问题', ['B'], []),
		);
		const { controller, submitMessage, setAutoFollow } = createController();
		const images: ChatAttachmentPayload[] = [{ media_type: 'image/png', data: 'image' }];
		const files: ChatFileAttachment[] = [
			{ filename: 'notes.txt', media_type: 'text/plain', data: 'file' },
		];
		controller.handleAskSelectionChange('ask-1', ['A']);
		controller.handleAskSelectionChange('ask-2', ['B']);

		controller.handleInputSubmit({ text: '请解释', images, files });

		expect(submitMessage).toHaveBeenCalledOnce();
		expect(submitMessage).toHaveBeenCalledWith(
			'关于「第一个问题」：A\n关于「第二个问题」：B 请解释',
			images,
			files,
		);
		expect(setAutoFollow).toHaveBeenCalled();
		expect(reducer.snapshot().interactions).toMatchObject({
			'ask-1': expect.objectContaining({ status: 'resolved' }),
			'ask-2': expect.objectContaining({ status: 'resolved' }),
		});
	});

	it('keeps ordinary composer input separate when the pending ask batch is incomplete', () => {
		loadAskMessages(createRequest('ask-1', '问题', ['A']));
		const { controller, submitMessage, setAutoFollow } = createController();
		controller.handleAskSelectionChange('ask-1', []);

		controller.handleInputSubmit({ text: '直接回复', images: [], files: [] });

		expect(submitMessage).toHaveBeenCalledOnce();
		expect(submitMessage).toHaveBeenCalledWith('直接回复', [], []);
		expect(reducer.snapshot().interactions?.['ask-1']).toMatchObject({ status: 'pending' });
		expect(setAutoFollow).toHaveBeenCalledOnce();
	});

	it('clears awaiting and locally resolved ask state when a session resumes', () => {
		loadAskMessages(createRequest('ask-1', '问题'));
		const { controller } = createController();

		controller.handleAskSelectionChange('ask-1', ['A']);
		controller.handleAskSubmit();
		controller.clearAskAwaiting(SESSION_ID);

		expect(reducer.snapshot().interactions?.['ask-1']).toBeUndefined();
		expect(reducer.getMessages(SESSION_ID)[0]).toMatchObject({
			awaiting: false,
			resolved: { answer: 'A' },
		});
		expect(controller.computeAskSelectionsReady()).toBe(false);
	});

	it('settles asks and clears only that session’s ask interactions atomically', () => {
		loadAskMessages(createRequest('ask-1', '问题'));
		const { controller } = createController();
		controller.handleIgnoreAsk('ask-1');

		for (const request of [
			{
				id: 'confirm-same',
				sessionId: SESSION_ID,
				owner: { kind: 'session' as const, sessionId: SESSION_ID },
				kind: 'confirm' as const,
				status: 'pending' as const,
				options: [],
				createdAt: '',
			},
			{
				id: 'scheduled-same',
				sessionId: SESSION_ID,
				owner: { kind: 'scheduled_tool_run' as const, toolRunId: 'toolrun-scheduled' },
				kind: 'scheduled_confirm' as const,
				status: 'pending' as const,
				options: [],
				createdAt: '',
			},
			{
				id: 'ask-other',
				sessionId: 'ses-other',
				owner: { kind: 'session' as const, sessionId: 'ses-other' },
				kind: 'ask' as const,
				status: 'pending' as const,
				options: [],
				createdAt: '',
			},
		])
			reducer.dispatch({ type: 'session/interaction-upserted', request });

		const observed: Array<{
			resolved: unknown;
			askInteraction: unknown;
		}> = [];
		let recording = false;
		const unsubscribe = reducer.subscribe((state) => {
			if (!recording) return;
			const askMessage = state.messages[SESSION_ID]?.find((message) => message.id === 'ask-1');
			observed.push({
				resolved: askMessage?.resolved,
				askInteraction: state.interactions['ask-1'],
			});
		});
		recording = true;

		controller.clearAskAwaiting(SESSION_ID);
		unsubscribe();

		expect(observed).toEqual([{ resolved: { ignored: true }, askInteraction: undefined }]);
		expect(reducer.snapshot().interactions).toMatchObject({
			'confirm-same': expect.objectContaining({ kind: 'confirm', status: 'pending' }),
			'scheduled-same': expect.objectContaining({ kind: 'scheduled_confirm', status: 'pending' }),
			'ask-other': expect.objectContaining({ sessionId: 'ses-other', kind: 'ask', status: 'pending' }),
		});
	});

	it('settles unanswered cards when a freeform answer resumes the session', () => {
		loadAskMessages(createRequest('ask-freeform', '问题'));
		const { controller } = createController();

		controller.clearAskAwaiting(SESSION_ID);

		expect(reducer.snapshot().interactions?.['ask-freeform']).toBeUndefined();
		expect(reducer.getMessages(SESSION_ID)[0]).toMatchObject({
			awaiting: false,
			resolved: null,
		});
	});
});
