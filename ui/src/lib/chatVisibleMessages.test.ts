import { describe, expect, it } from 'vitest';
import {
	DRAFT_SESSION_ID,
	initialSessionState,
	type SessionMessage,
	type SessionReducerState,
} from './sessionReducer.ts';
import type { InteractionRequest } from './contracts/app.ts';
import { projectChatVisibleMessages, selectChatVisibleMessages } from './chatVisibleMessages.ts';

const stateWith = (partial: Partial<SessionReducerState>): SessionReducerState => ({
	...initialSessionState,
	...partial,
});

const askMessage = (id: string, content = '选择方案'): SessionMessage => ({
	id,
	role: 'assistant',
	type: 'ask',
	content,
	toolName: 'ask',
});

const askInteraction = (
	id: string,
	status: InteractionRequest['status'],
	options: string[],
	response?: unknown,
): InteractionRequest => ({
	id,
	sessionId: 'ses-1',
	kind: 'ask',
	status,
	prompt: '请选择',
	options,
	createdAt: '2026-09-24T00:00:00.000Z',
	...(response === undefined ? {} : { response }),
});

describe('selectChatVisibleMessages', () => {
	it('projects ask state from the selected transcript and interaction slices', () => {
		const ask = askMessage('step-active');
		const selected = projectChatVisibleMessages([ask], {
			'step-active': askInteraction('step-active', 'pending', ['继续']),
		});

		expect(selected).toEqual([{ ...ask, options: ['继续'], awaiting: true, resolved: null }]);
	});

	it('returns ordinary messages unchanged', () => {
		const ordinary: SessionMessage = {
			id: 'msg-1',
			role: 'assistant',
			type: 'text',
			content: '完成',
		};
		const selected = selectChatVisibleMessages(
			stateWith({ messages: { 'ses-1': [ordinary] } }),
			'ses-1',
		);

		expect(selected).toHaveLength(1);
		expect(selected[0]).toBe(ordinary);
	});

	it('selects draft messages when there is no active session', () => {
		const draft: SessionMessage = { id: 'msg-draft', role: 'user', content: '草稿' };
		const other: SessionMessage = { id: 'msg-other', role: 'user', content: '其他' };
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: {
					[DRAFT_SESSION_ID]: [draft],
					'ses-1': [other],
				},
			}),
			null,
		);

		expect(selected).toEqual([draft]);
		expect(selected[0]).toBe(draft);
	});

	it('leaves ask messages unchanged when no interaction matches the message id', () => {
		const ask = askMessage('step-1');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-1': [ask] },
				interactions: {
					'step-other': askInteraction('step-other', 'pending', ['A']),
				},
			}),
			'ses-1',
		);

		expect(selected[0]).toBe(ask);
	});

	it('projects pending ask options and awaiting state by message id', () => {
		const ask = { ...askMessage('step-1'), options: ['stale'], awaiting: false };
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-1': [ask] },
				interactions: {
					'step-1': askInteraction('step-1', 'pending', ['方案 A', '方案 B']),
				},
			}),
			'ses-1',
		);

		expect(selected[0]).toEqual({
			...ask,
			options: ['方案 A', '方案 B'],
			awaiting: true,
			resolved: null,
		});
	});

	it('projects a resolved answer', () => {
		const ask = askMessage('step-1');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-1': [ask] },
				interactions: {
					'step-1': askInteraction('step-1', 'resolved', ['A', 'B'], {
						answer: '方案 B',
					}),
				},
			}),
			'ses-1',
		);

		expect(selected[0]).toMatchObject({
			options: ['A', 'B'],
			awaiting: false,
			resolved: { answer: '方案 B' },
		});
	});

	it('projects a resolved ignored interaction', () => {
		const ask = askMessage('step-1');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-1': [ask] },
				interactions: {
					'step-1': askInteraction('step-1', 'resolved', ['A'], { ignored: true }),
				},
			}),
			'ses-1',
		);

		expect(selected[0]).toMatchObject({
			options: ['A'],
			awaiting: false,
			resolved: { ignored: true },
		});
	});
});
