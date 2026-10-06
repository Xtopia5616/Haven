import { describe, expect, it } from 'vitest';
import {
	pickContinueStrategy,
	shouldResubmitOriginalUser,
	shouldShowContinueButton,
} from './continueSession.ts';

describe('shouldShowContinueButton', () => {
	it('shows for a conversation whose last visible message is from the user', () => {
		expect(
			shouldShowContinueButton([
				{ role: 'assistant', content: '已完成' },
				{ role: 'user', content: '继续做' },
			]),
		).toBe(true);
	});

	it('does not show for an assistant tail unless the session errored', () => {
		expect(shouldShowContinueButton([{ role: 'assistant', content: '已完成' }])).toBe(false);
		expect(shouldShowContinueButton([{ role: 'assistant', content: '中断' }], true)).toBe(true);
	});

	it('keeps the error affordance available even when no message was persisted', () => {
		expect(shouldShowContinueButton([], true)).toBe(true);
	});
});

describe('pickContinueStrategy', () => {
	it('resends the original user text when the agent never generated', () => {
		const strategy = pickContinueStrategy([
			{ role: 'user', content: '帮我打开计算器', id: 'msg-1' },
		]);
		expect(strategy).toEqual({
			mode: 'resend_user',
			text: '帮我打开计算器',
			messageId: 'msg-1',
		});
	});

	it('ignores trailing supplement badges when deciding resend', () => {
		const strategy = pickContinueStrategy([
			{ role: 'user', content: '改一下标题', id: 'msg-1' },
			{ role: 'assistant', type: 'supplement', content: '改一下标题' },
		]);
		expect(strategy).toEqual({
			mode: 'resend_user',
			text: '改一下标题',
			messageId: 'msg-1',
		});
	});

	it('sends 继续 when the LLM was interrupted mid-generation', () => {
		const strategy = pickContinueStrategy([
			{ role: 'user', content: '写一首诗', id: 'msg-1' },
			{ role: 'assistant', type: 'reasoning', content: '先构思意境…' },
			{ role: 'assistant', type: 'thought', content: '床前明月' },
		]);
		expect(strategy).toEqual({ mode: 'continue', text: '继续' });
	});

	it('sends 继续 when a tool card already ran after the user turn', () => {
		const strategy = pickContinueStrategy([
			{ role: 'user', content: '查天气', id: 'msg-1' },
			{ role: 'assistant', type: 'tool', toolName: 'shell', content: 'ok' },
		]);
		expect(strategy).toEqual({ mode: 'continue', text: '继续' });
	});

	it('falls back to 继续 when there is no usable user text', () => {
		expect(pickContinueStrategy([])).toEqual({ mode: 'continue', text: '继续' });
		expect(pickContinueStrategy([{ role: 'user', content: '   ' }])).toEqual({
			mode: 'continue',
			text: '继续',
		});
	});
});

describe('shouldResubmitOriginalUser', () => {
	it('does not resubmit when the persisted user turn survived truncate', () => {
		expect(
			shouldResubmitOriginalUser(
				[{ role: 'user', content: '帮我打开计算器', id: 'msg-abc' }],
				'msg-abc',
			),
		).toBe(false);
	});

	it('resubmits when the original turn is missing after resync', () => {
		expect(shouldResubmitOriginalUser([], 'msg-original')).toBe(true);
		expect(
			shouldResubmitOriginalUser(
				[{ role: 'user', content: '帮我打开计算器', id: 'msg-other' }],
				'msg-original',
			),
		).toBe(true);
	});

	it('resubmits optimistic-only bubbles that never got a msg- id', () => {
		expect(
			shouldResubmitOriginalUser(
				[{ role: 'user', content: '帮我打开计算器', id: '171000-u-ab12' }],
				'local-optimistic-id',
			),
		).toBe(true);
	});
});
