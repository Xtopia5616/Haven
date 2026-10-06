import { describe, expect, it } from 'vitest';
import {
	groupConversationMessages,
	groupConversationTimeline,
	firstWaitingBackgroundActionId,
	isMergedConversationMessage,
	type ConversationMessage,
} from './conversationTimeline.ts';

const message = (id: string, extra: Partial<ConversationMessage> = {}): ConversationMessage => ({
	id,
	...extra,
});

describe('conversationTimeline grouping', () => {
	it('merges adjacent agent work and keeps user-facing messages separate', () => {
		const items = groupConversationMessages([
			message('user-1', { type: null }),
			message('thought-1', { type: 'thought', stepNumber: 1 }),
			message('tool-1', { type: 'tool', toolName: 'files', stepNumber: 1 }),
			message('reasoning-1', { type: 'reasoning', stepNumber: 2 }),
			message('answer-1', { type: null }),
		]);

		expect(items).toHaveLength(3);
		expect(items[0]).toMatchObject({ kind: 'message', message: { id: 'user-1' } });
		expect(items[1]).toMatchObject({
			kind: 'activity',
			id: 'activity-user-1-step-1',
			streaming: false,
			toolCount: 1,
			stepCount: 2,
		});
		if (items[1].kind === 'activity') {
			expect(items[1].entries.map(({ message: entry }) => entry.id)).toEqual([
				'thought-1',
				'tool-1',
				'reasoning-1',
			]);
		}
		expect(items[2]).toMatchObject({ kind: 'message', message: { id: 'answer-1' } });
	});

	it('keeps a work group active while any entry is streaming', () => {
		const items = groupConversationMessages([
			message('thought-1', { type: 'thought', streaming: false }),
			message('tool-1', { type: 'tool', streaming: true }),
		]);

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({ kind: 'activity', streaming: true, toolCount: 1 });
	});

	it('does not hide ask cards inside a work group', () => {
		const items = groupConversationMessages([
			message('tool-1', { type: 'tool' }),
			message('ask-1', { type: 'ask' }),
			message('tool-2', { type: 'tool' }),
		]);

		expect(items.map((item) => item.kind)).toEqual(['activity', 'message', 'activity']);
		expect(items[1]).toMatchObject({ kind: 'message', message: { id: 'ask-1' } });
	});

	it('only merges thought, reasoning and tool messages', () => {
		expect(isMergedConversationMessage(message('thought', { type: 'thought' }))).toBe(true);
		expect(isMergedConversationMessage(message('reasoning', { type: 'reasoning' }))).toBe(true);
		expect(isMergedConversationMessage(message('tool', { type: 'tool' }))).toBe(true);
		expect(isMergedConversationMessage(message('ask', { type: 'ask' }))).toBe(false);
		expect(isMergedConversationMessage(message('text', { type: null }))).toBe(false);
	});

	it('projects background status into its tool call and anchors scheduled cards', () => {
		const messages = [
			message('tool-background', {
				type: 'tool',
				stepNumber: 3,
				content: '{"background":true,"action_id":"act-background"}',
			}),
			message('tool-schedule', {
				type: 'tool',
				stepNumber: 4,
				toolName: 'schedule.set',
				content: '{"operation":"set","id":"act-scheduled"}',
			}),
		];
		const actions = [
			{
				id: 'act-background',
				kind: 'background' as const,
				status: 'running' as const,
				sessionId: 'ses-1',
			},
			{
				id: 'act-scheduled',
				kind: 'scheduled' as const,
				status: 'waiting' as const,
				sessionId: 'ses-1',
			},
		];
		const items = groupConversationTimeline(messages, {
			actions,
			awaitingBackground: true,
			awaitingBackgroundCount: 1,
		});

		expect(items.map((item) => item.kind)).toEqual(['activity', 'action']);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [
				{ message: { id: 'tool-background' } },
				{ message: { id: 'tool-schedule' } },
			],
		});
		expect(items[1]).toMatchObject({
			kind: 'action',
			action: { id: 'act-scheduled', kind: 'scheduled' },
			awaitingBackgroundResult: false,
		});
	});

	it('prefers sourceStepId over observation lookup', () => {
		const messages = [
			message('observation-anchor', {
				type: 'tool',
				stepNumber: 2,
				content: '{"background":true,"action_id":"act-source"}',
			}),
			message('message-boundary'),
			message('stable-source-step', {
				type: 'tool',
				stepNumber: 3,
				content: 'background task result unavailable',
			}),
		];
		const items = groupConversationTimeline(messages, {
			actions: [{
				id: 'act-source',
				kind: 'background',
				sourceStepId: 'stable-source-step',
			}],
		});

		expect(items.map((item) => item.kind)).toEqual(['activity', 'message', 'activity']);
		expect(items[2]).toMatchObject({ kind: 'activity', entries: [{ message: { id: 'stable-source-step' } }] });
	});

	it('keeps unanchored actions in the owning session timeline and emits one wait fallback', () => {
		const action = {
			id: 'act-unanchored',
			kind: 'background' as const,
			status: 'running' as const,
			sessionId: 'ses-1',
		};
		const items = groupConversationTimeline([message('user-1')], {
			actions: [action],
			awaitingBackground: true,
			awaitingBackgroundCount: 0,
		});

		expect(items.map((item) => item.kind)).toEqual(['message', 'action']);
		expect(items[1]).toMatchObject({
			kind: 'action',
			action: { id: 'act-unanchored' },
			awaitingBackgroundResult: true,
		});

		const fallback = groupConversationTimeline([], { awaitingBackground: true });
		expect(fallback).toEqual([
			{
				kind: 'action-wait',
				id: 'awaiting-background-result',
				awaitingBackgroundCount: 0,
			},
		]);
	});

	it('chooses the same earliest running background action for the wait indicator', () => {
		expect(
			firstWaitingBackgroundActionId(
				[
					{ id: 'act-later', kind: 'background', status: 'running', startedAt: '2026-10-06T11:00:00Z' },
					{ id: 'act-scheduled', kind: 'scheduled', status: 'waiting' },
					{ id: 'act-earlier', kind: 'background', status: 'running', startedAt: '2026-10-06T10:00:00Z' },
				],
				true,
			),
		).toBe('act-earlier');
		expect(firstWaitingBackgroundActionId([], false)).toBeNull();
	});

	it('folds a terminal background result into its source tool card without a duplicate Action card', () => {
		const action = {
			id: 'act-finished',
			kind: 'background' as const,
			status: 'completed' as const,
			sessionId: 'ses-1',
			output: 'same result',
		};
		const transcriptResult = message('tool-finished', {
			type: 'tool',
			actionId: null,
			sourceActionId: action.id,
			content: JSON.stringify({
				background: true,
				action_id: action.id,
				status: 'completed',
				output: action.output,
			}),
		});
		const items = groupConversationTimeline([transcriptResult], { actions: [action] });

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [{ message: { id: 'tool-finished', sourceActionId: action.id } }],
		});
	});

	it('keeps terminal detail on the source tool card until the transcript receives it', () => {
		const action = {
			id: 'act-unprojected',
			kind: 'background' as const,
			status: 'failed' as const,
			sessionId: 'ses-1',
			error: 'process failed',
		};
		const source = message('tool-unprojected', {
			type: 'tool',
			actionId: action.id,
			sourceActionId: action.id,
			content: '{"background":true,"status":"running"}',
		});
		const items = groupConversationTimeline([source], { actions: [action] });

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [{ message: { id: 'tool-unprojected', actionId: action.id } }],
		});
	});
});
