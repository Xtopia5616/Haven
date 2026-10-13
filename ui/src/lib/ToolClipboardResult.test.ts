import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ToolClipboardResult from './ToolClipboardResult.svelte';

describe('ToolClipboardResult', () => {
	it('keeps history rows associated with entry_id when results are reordered', () => {
		const first = {
			entry_id: 'clip-00000000000000000000000000000001',
			content: 'copied first',
		};
		const second = {
			entry_id: 'clip-00000000000000000000000000000002',
			content: 'copied second',
		};
		const { rerender } = render(ToolClipboardResult, {
			data: { entries: [first, second], total: 2 },
		});
		const firstRow = screen.getByText(first.content).closest('.tool-result-search-row');

		rerender({ data: { entries: [second, first], total: 2 } });

		expect(screen.getByText(first.content).closest('.tool-result-search-row')).toBe(firstRow);
	});
});
