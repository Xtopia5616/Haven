import { fireEvent, render, screen } from '@testing-library/svelte';
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

	it('keeps batch disclosure state when the step preamble is reconciled', async () => {
		const tools = [
			{
				id: 'step-a',
				role: 'assistant',
				content: 'first result',
				type: 'tool',
				toolName: 'shell',
				stepNumber: 7,
				streaming: false,
			},
			{
				id: 'step-b',
				role: 'assistant',
				content: 'second result',
				type: 'tool',
				toolName: 'files',
				stepNumber: 7,
				streaming: false,
			},
		];
		const { container, rerender } = render(ConversationTimeline, { messages: tools });
		const headers = () =>
			Array.from(container.querySelectorAll('.md-collapsible-header')) as HTMLButtonElement[];

		await fireEvent.click(headers()[0]);
		await fireEvent.click(headers()[1]);
		expect(headers()[0].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');

		await rerender({
			messages: [
				{
					id: 'msg-preamble',
					role: 'assistant',
					content: '检查相关文件',
					type: 'thought',
					stepNumber: 7,
					streaming: false,
				},
				...tools,
			],
		});

		expect(headers()[0].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');
	});
});
