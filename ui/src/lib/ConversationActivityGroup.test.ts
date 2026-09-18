import { describe, expect, it } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import ConversationActivityGroup from './ConversationActivityGroup.svelte';

const entry = (streaming: boolean) => ({
	message: {
		id: 'thought-1',
		role: 'assistant',
		content: streaming ? '正在检查' : '已检查完成',
		type: 'thought',
		streaming,
	},
	index: 0,
});

describe('ConversationActivityGroup', () => {
	it('stays open while active and auto-collapses when work completes', async () => {
		const props = { entries: [entry(true)], streaming: true, stepCount: 1 };
		const { container, rerender } = render(ConversationActivityGroup, props);
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;

		expect(header.getAttribute('aria-expanded')).toBe('true');
		expect(container.textContent).toContain('正在整理下一步');

		await rerender({ entries: [entry(false)], streaming: false, stepCount: 1 });

		expect(header.getAttribute('aria-expanded')).toBe('false');
		expect(container.textContent).toContain('已完成工作过程');
	});

	it('preserves a manual expansion after completion', async () => {
		const props = { entries: [entry(false)], streaming: false, stepCount: 1 };
		const { container, rerender } = render(ConversationActivityGroup, props);
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;

		expect(header.getAttribute('aria-expanded')).toBe('false');
		await fireEvent.click(header);
		expect(header.getAttribute('aria-expanded')).toBe('true');

		await rerender({ entries: [entry(false)], streaming: false, stepCount: 1 });
		expect(header.getAttribute('aria-expanded')).toBe('true');
		expect(container.textContent).toContain('已检查完成');
	});

	it('preserves each nested disclosure when the activity summary is toggled', async () => {
		const entries = [
			{
				message: {
					id: 'reasoning-1',
					role: 'assistant',
					content: '思考完成',
					type: 'reasoning',
					streaming: false,
				},
				index: 0,
			},
			{
				message: {
					id: 'tool-1',
					role: 'assistant',
					content: '工具完成',
					type: 'tool',
					toolName: 'shell',
					streaming: false,
				},
				index: 1,
			},
		];
		const { container } = render(ConversationActivityGroup, {
			entries,
			streaming: false,
			toolCount: 1,
			stepCount: 2,
			allMessages: entries.map((entry) => entry.message),
		});
		const headers = () =>
			Array.from(container.querySelectorAll('.md-collapsible-header')) as HTMLButtonElement[];
		const outer = headers()[0];

		expect(headers()).toHaveLength(3);
		await fireEvent.click(outer);
		await fireEvent.click(headers()[1]);
		await fireEvent.click(headers()[2]);
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[2].getAttribute('aria-expanded')).toBe('true');

		await fireEvent.click(outer);
		await fireEvent.click(outer);
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[2].getAttribute('aria-expanded')).toBe('true');
	});

	it('keeps a running tool card manually collapsible across live output updates', async () => {
		const message = (content: string) => ({
			id: 'tool-running',
			role: 'assistant',
			content,
			type: 'tool',
			toolName: 'shell',
			streaming: true,
		});
		const { container, rerender } = render(ConversationActivityGroup, {
			entries: [{ message: message('第一段输出'), index: 0 }],
			streaming: true,
			toolCount: 1,
			stepCount: 1,
		});
		const headers = () =>
			Array.from(container.querySelectorAll('.md-collapsible-header')) as HTMLButtonElement[];
		const toolHeader = headers()[1];

		expect(toolHeader.getAttribute('aria-expanded')).toBe('true');
		await fireEvent.click(toolHeader);
		expect(toolHeader.getAttribute('aria-expanded')).toBe('false');

		await rerender({
			entries: [{ message: message('第二段输出'), index: 0 }],
			streaming: true,
			toolCount: 1,
			stepCount: 1,
		});
		expect(headers()[1].getAttribute('aria-expanded')).toBe('false');

		await fireEvent.click(headers()[1]);
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');
	});

	it('keeps the collapsible work surface outlined around nested work entries', () => {
		const toolMessage = {
			...entry(false).message,
			type: 'tool',
			toolName: 'shell',
			content: 'stdout output',
		};
		const { container } = render(ConversationActivityGroup, {
			entries: [{ message: toolMessage, index: 0 }],
			streaming: false,
			toolCount: 1,
			stepCount: 1,
		});
		const group = container.querySelector('.activity-group') as HTMLElement;

		expect(group.getAttribute('data-surface')).toBe('outlined');
	});
});
