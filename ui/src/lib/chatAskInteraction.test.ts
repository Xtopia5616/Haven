import { beforeEach, describe, expect, it, vi } from 'vitest';
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
		expect(reducer.getState().interactions).toMatchObject({
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

	it('routes composer input through a fully selected ask batch and appends typed text', () => {
		loadAskMessages(
			// Quick choices come from the live tool observation; the interaction
			// event that gates the ask carries no option list.
			createRequest('ask-1', '第一个问题', ['A'], []),
			createRequest('ask-2', '第二个问题', ['B'], []),
		);
		const { controller, submitMessage, setAutoFollow } = createController();
		const images = [{ data: 'image' }];
		const files = [{ filename: 'notes.txt' }];
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
		expect(reducer.getState().interactions).toMatchObject({
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
		expect(reducer.getState().interactions?.['ask-1']).toMatchObject({ status: 'pending' });
		expect(setAutoFollow).toHaveBeenCalledOnce();
	});

	it('clears awaiting and locally resolved ask state when a session resumes', () => {
		loadAskMessages(createRequest('ask-1', '问题'));
		const { controller } = createController();

		controller.handleAskSelectionChange('ask-1', ['A']);
		controller.clearAskAwaiting(SESSION_ID);

		expect(reducer.getState().interactions?.['ask-1']).toBeUndefined();
		expect(controller.computeAskSelectionsReady()).toBe(false);
	});
});
