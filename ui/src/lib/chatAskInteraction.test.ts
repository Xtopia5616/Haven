import { describe, expect, it, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { createAskInteractionController } from './chatAskInteraction.ts';
import { sessionMessagesStore, setSessionMessages } from './sessionMessages.ts';

const SESSION_ID = 'ses-ask';

function createController(submitMessage = vi.fn()) {
	const ready = vi.fn();
	const controller = createAskInteractionController({
		getActiveSessionId: () => SESSION_ID,
		setAutoFollow: vi.fn(),
		setSelectionsReady: ready,
		submitMessage,
	});
	return { controller, ready, submitMessage };
}

describe('createAskInteractionController', () => {
	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	it('requires every awaiting ask to be selected before batch submit', () => {
		setSessionMessages(SESSION_ID, [
			{ id: 'ask-1', type: 'ask', content: '第一个问题', awaiting: true },
			{ id: 'ask-2', type: 'ask', content: '第二个问题', awaiting: true },
		]);
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
		expect(get(sessionMessagesStore)[SESSION_ID]).toEqual([
			expect.objectContaining({ id: 'ask-1', awaiting: false, resolved: { answer: 'A' } }),
			expect.objectContaining({ id: 'ask-2', awaiting: false, resolved: { answer: 'B' } }),
		]);
	});

	it('submits an ignored ask once and rejects duplicate resolution', () => {
		setSessionMessages(SESSION_ID, [
			{ id: 'ask-1', type: 'ask', content: '要继续吗？', awaiting: true },
		]);
		const { controller, submitMessage } = createController();

		controller.handleIgnoreAsk('ask-1');
		controller.handleIgnoreAsk('ask-1');

		expect(submitMessage).toHaveBeenCalledOnce();
		expect(submitMessage).toHaveBeenCalledWith('忽略', [], []);
	});

	it('clears awaiting and locally resolved ask state when a session resumes', () => {
		setSessionMessages(SESSION_ID, [
			{ id: 'ask-1', type: 'ask', content: '问题', awaiting: true, resolved: { answer: 'A' } },
		]);
		const { controller } = createController();

		controller.handleAskSelectionChange('ask-1', ['A']);
		controller.clearAskAwaiting(SESSION_ID);

		expect(get(sessionMessagesStore)[SESSION_ID][0]).toMatchObject({
			awaiting: false,
			resolved: null,
		});
		expect(controller.computeAskSelectionsReady()).toBe(false);
	});
});
