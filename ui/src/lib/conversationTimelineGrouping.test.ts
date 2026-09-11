import { describe, expect, it } from 'vitest';
import {
	groupConversationMessages,
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
			id: 'activity-thought-1',
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
});
