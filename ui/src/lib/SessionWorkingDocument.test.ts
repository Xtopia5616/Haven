import { render, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';

const markdown = vi.hoisted(() => ({
	getMarkdownRenderer: vi.fn(),
	renderMarkdown: vi.fn(),
}));

vi.mock('$lib/markdownRenderer.ts', () => markdown);

import SessionWorkingDocument from './SessionWorkingDocument.svelte';

describe('SessionWorkingDocument', () => {
	it('renders the current Markdown through the shared renderer', async () => {
		markdown.getMarkdownRenderer.mockResolvedValue(undefined);
		markdown.renderMarkdown.mockImplementation((source: string) => `<h1>${source}</h1>`);
		const { container, rerender } = render(SessionWorkingDocument, {
			markdown: '# Goal',
		});
		await waitFor(() =>
			expect(container.querySelector('.working-document-content h1')?.textContent).toBe(
				'# Goal',
			),
		);

		await rerender({ markdown: '# Next action' });
		await waitFor(() =>
			expect(container.querySelector('.working-document-content h1')?.textContent).toBe(
				'# Next action',
			),
		);
		expect(markdown.renderMarkdown).toHaveBeenLastCalledWith('# Next action');
	});
});
