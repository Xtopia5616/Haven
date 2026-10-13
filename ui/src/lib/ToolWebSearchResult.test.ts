import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ToolWebSearchResult from './ToolWebSearchResult.svelte';

describe('ToolWebSearchResult', () => {
	it('keeps citation rows associated with their tuple when distinct results are reordered', () => {
		const first = { url: 'https://example.test/first', title: 'First result', snippet: 'first' };
		const second = { url: 'https://example.test/second', title: 'Second result', snippet: 'second' };
		const { rerender } = render(ToolWebSearchResult, { data: { results: [first, second] } });
		const firstRow = screen.getByText(first.title).closest('.tool-result-search-row');

		rerender({ data: { results: [second, first] } });

		expect(screen.getByText(first.title).closest('.tool-result-search-row')).toBe(firstRow);
	});

	it('renders repeated citation tuples as distinct rows', () => {
		const repeated = { url: 'https://example.test/same', title: 'Same result', snippet: 'same' };
		render(ToolWebSearchResult, { data: { results: [repeated, repeated] } });

		expect(screen.getAllByText(repeated.title)).toHaveLength(2);
	});
});
