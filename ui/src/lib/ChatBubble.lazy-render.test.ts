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
});
