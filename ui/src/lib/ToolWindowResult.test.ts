import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ToolWindowResult from './ToolWindowResult.svelte';

describe('ToolWindowResult', () => {
	it('keeps window rows associated with window_id when reordered', () => {
		const first = { window_id: 'hwnd:1', title: 'First window' };
		const second = { window_id: 'hwnd:2', title: 'Second window' };
		const { rerender } = render(ToolWindowResult, { data: { windows: [first, second] } });
		const firstRow = screen.getByText(first.title).closest('.tool-result-window-row');

		rerender({ data: { windows: [second, first] } });

		expect(screen.getByText(first.title).closest('.tool-result-window-row')).toBe(firstRow);
	});
});
