import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ConversationTimeline from './ConversationTimeline.svelte';

describe('ConversationTimeline', () => {
	it('renders the message timeline immediately when the first message arrives', () => {
		render(ConversationTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '你好', type: 'user' }],
		});

		expect(document.querySelector('.message-list')).toBeTruthy();
		expect(document.querySelector('.bubble.user')?.textContent).toContain('你好');
	});

	it('passes through the continue action for a user-tail conversation', () => {
		render(ConversationTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '继续处理', type: 'user' }],
			showContinueButton: true,
		});

		expect(screen.getByRole('button', { name: '继续生成' })).toBeTruthy();
		expect(document.querySelector('.continue-action')).toBeTruthy();
		expect(
			screen.getByRole('button', { name: '继续生成' }).classList.contains('md-btn--outlined'),
		).toBe(true);
		expect(screen.getByRole('button', { name: '继续生成' }).querySelector('svg')).toBeNull();
	});

	it('removes the continue action while it is unavailable', () => {
		render(ConversationTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '继续处理', type: 'user' }],
			showContinueButton: true,
			continueDisabled: true,
		});

		expect(screen.queryByRole('button', { name: '继续生成' })).toBeNull();
		expect(document.querySelector('.continue-action')).toBeNull();
	});
});
