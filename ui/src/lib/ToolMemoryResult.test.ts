import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ToolMemoryResult from './ToolMemoryResult.svelte';

describe('ToolMemoryResult', () => {
	it('keeps fact rows associated with fact_id when results are reordered', () => {
		const first = {
			fact_id: 'fact-00000000000000000000000000000001',
			subject: 'Alice',
			predicate: 'likes',
			object: 'tea',
		};
		const second = {
			fact_id: 'fact-00000000000000000000000000000002',
			subject: 'Bob',
			predicate: 'likes',
			object: 'coffee',
		};
		const { rerender } = render(ToolMemoryResult, { data: { facts: [first, second] } });
		const firstRow = screen.getByText('Alice').closest('.memory-row');

		rerender({ data: { facts: [second, first] } });

		expect(screen.getByText('Alice').closest('.memory-row')).toBe(firstRow);
	});
});
