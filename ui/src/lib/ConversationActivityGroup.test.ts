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
});
