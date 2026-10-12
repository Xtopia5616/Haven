import { describe, expect, it } from 'vitest';
import {
	DRAFT_SESSION_ID,
	initialSessionState,
	type SessionMessage,
	type SessionReducerState,
} from './sessionReducer.ts';
import type { AskResponseView, InteractionRequest } from './contracts/app.ts';
import { projectChatVisibleMessages, selectChatVisibleMessages } from './chatVisibleMessages.ts';

const stateWith = (partial: Partial<SessionReducerState>): SessionReducerState => ({
	...initialSessionState,
	...partial,
});

const askMessage = (id: string, content = '选择方案'): SessionMessage => ({
	id,
	role: 'assistant' as const,
	type: 'ask',
	content,
	toolName: 'ask',
});

const askInteraction = (
	id: string,
	status: InteractionRequest['status'],
	options: string[],
	response?: AskResponseView,
): InteractionRequest => ({
	requestId: id,
	sessionId: 'ses-00000000000000000000000000000001',
	owner: { kind: 'session', sessionId: 'ses-00000000000000000000000000000001' },
	kind: 'ask',
	status,
	options,
	createdAt: '2026-09-24T00:00:00.000Z',
	...(response === undefined ? {} : { response }),
});

describe('selectChatVisibleMessages', () => {
	it('projects ask state from the selected transcript and interaction slices', () => {
		const ask = askMessage('step-0c476b16c4fee42acc27413afcc2c254');
		const selected = projectChatVisibleMessages([ask], {
			'step-0c476b16c4fee42acc27413afcc2c254': askInteraction('step-0c476b16c4fee42acc27413afcc2c254', 'pending', ['继续']),
		});

		expect(selected).toEqual([{ ...ask, options: ['继续'], awaiting: true, resolved: null }]);
	});

	it('returns ordinary messages unchanged', () => {
		const ordinary: SessionMessage = {
			id: 'msg-00000000000000000000000000000001',
			role: 'assistant' as const,
			content: '完成',
		};
		const selected = selectChatVisibleMessages(
			stateWith({ messages: { 'ses-00000000000000000000000000000001': [ordinary] } }),
			'ses-00000000000000000000000000000001',
		);

		expect(selected).toHaveLength(1);
		expect(selected[0]).toBe(ordinary);
	});

	it('selects draft messages when there is no active session', () => {
		const draft: SessionMessage = { id: 'msg-draft', role: 'user' as const, content: '草稿' };
		const other: SessionMessage = { id: 'msg-3bd93237c58257fb6f9a869f6125a824', role: 'user' as const, content: '其他' };
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: {
					[DRAFT_SESSION_ID]: [draft],
					'ses-00000000000000000000000000000001': [other],
				},
			}),
			null,
		);

		expect(selected).toEqual([draft]);
		expect(selected[0]).toBe(draft);
	});

	it('leaves ask messages unchanged when no interaction matches the message id', () => {
		const ask = askMessage('step-00000000000000000000000000000001');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-00000000000000000000000000000001': [ask] },
				interactions: {
					'step-4626a4fa5daedbd40725050ffb1f4e74': askInteraction('step-4626a4fa5daedbd40725050ffb1f4e74', 'pending', ['A']),
				},
			}),
			'ses-00000000000000000000000000000001',
		);

		expect(selected[0]).toBe(ask);
	});

	it('projects pending ask options and awaiting state by message id', () => {
		const ask = { ...askMessage('step-00000000000000000000000000000001'), options: ['stale'], awaiting: false };
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-00000000000000000000000000000001': [ask] },
				interactions: {
					'step-00000000000000000000000000000001': askInteraction('step-00000000000000000000000000000001', 'pending', ['方案 A', '方案 B']),
				},
			}),
			'ses-00000000000000000000000000000001',
		);

		expect(selected[0]).toEqual({
			...ask,
			options: ['方案 A', '方案 B'],
			awaiting: true,
			resolved: null,
		});
	});

	it('preserves live quick choices when the pending interaction has no options', () => {
		const ask = { ...askMessage('step-00000000000000000000000000000001'), options: ['方案 A', '方案 B'], awaiting: true };
		const selected = projectChatVisibleMessages([ask], {
			'step-00000000000000000000000000000001': askInteraction('step-00000000000000000000000000000001', 'pending', []),
		});

		expect(selected[0]).toMatchObject({
			options: ['方案 A', '方案 B'],
			awaiting: true,
		});
	});

	it('projects a resolved answer', () => {
		const ask = askMessage('step-00000000000000000000000000000001');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-00000000000000000000000000000001': [ask] },
				interactions: {
					'step-00000000000000000000000000000001': askInteraction('step-00000000000000000000000000000001', 'resolved', ['A', 'B'], {
						answer: '方案 B',
					}),
				},
			}),
			'ses-00000000000000000000000000000001',
		);

		expect(selected[0]).toMatchObject({
			options: ['A', 'B'],
			awaiting: false,
			resolved: { answer: '方案 B' },
		});
	});

	it('projects a resolved ignored interaction', () => {
		const ask = askMessage('step-00000000000000000000000000000001');
		const selected = selectChatVisibleMessages(
			stateWith({
				messages: { 'ses-00000000000000000000000000000001': [ask] },
				interactions: {
					'step-00000000000000000000000000000001': askInteraction('step-00000000000000000000000000000001', 'resolved', ['A'], { ignored: true }),
				},
			}),
			'ses-00000000000000000000000000000001',
		);

		expect(selected[0]).toMatchObject({
			options: ['A'],
			awaiting: false,
			resolved: { ignored: true },
		});
	});
});
