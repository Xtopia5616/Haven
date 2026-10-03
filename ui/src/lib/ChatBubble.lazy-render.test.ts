import { render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';

const markdown = vi.hoisted(() => ({
	getMarkdownRenderer: vi.fn(),
	renderMarkdown: vi.fn(),
}));

vi.mock('$lib/markdownRenderer.ts', () => markdown);

import ChatBubble from './ChatBubble.svelte';

describe('ChatBubble lazy Markdown rendering', () => {
	it('keeps rendering new streamed content after the shared renderer loads', async () => {
		let resolveRenderer!: () => void;
		markdown.getMarkdownRenderer.mockImplementation(
			() =>
				new Promise<void>((resolve) => {
					resolveRenderer = resolve;
				}),
		);
		markdown.renderMarkdown.mockImplementation((text: string) => `<strong>${text}</strong>`);

		const { container, rerender } = render(ChatBubble, {
			type: null,
			time: null,
			role: 'assistant',
			content: 'first chunk',
			streaming: true,
		});

		await waitFor(() => expect(markdown.getMarkdownRenderer).toHaveBeenCalledOnce());
		await rerender({ content: 'second chunk' });
		resolveRenderer();
		await waitFor(() => {
			expect(container.querySelector('.md-content strong')?.textContent).toBe('second chunk');
		});

		await rerender({ content: 'third chunk' });
		await waitFor(() => {
			expect(container.querySelector('.md-content strong')?.textContent).toBe('third chunk');
		});
		expect(markdown.renderMarkdown).toHaveBeenLastCalledWith('third chunk', true);
	});

	it('keeps a text selection stable while streamed Markdown is waiting to rerender', async () => {
		markdown.getMarkdownRenderer.mockResolvedValue(undefined);
		markdown.renderMarkdown.mockImplementation((text: string) => `<strong>${text}</strong>`);

		const { container, rerender } = render(ChatBubble, {
			type: null,
			time: null,
			role: 'assistant',
			content: 'first chunk',
			streaming: true,
		});
		await waitFor(() =>
			expect(container.querySelector('.md-content strong')?.textContent).toBe('first chunk'),
		);

		const textNode = container.querySelector('.md-content strong')?.firstChild;
		if (!textNode) throw new Error('Expected rendered Markdown text');
		const range = document.createRange();
		range.setStart(textNode, 0);
		range.setEnd(textNode, 5);
		window.getSelection()?.addRange(range);

		await rerender({ content: 'second chunk' });
		expect(container.querySelector('.md-content strong')?.textContent).toBe('first chunk');
		expect(window.getSelection()?.toString()).toBe('first');

		window.getSelection()?.removeAllRanges();
		document.dispatchEvent(new Event('selectionchange'));
		await waitFor(() =>
			expect(container.querySelector('.md-content strong')?.textContent).toBe('second chunk'),
		);
	});
});
