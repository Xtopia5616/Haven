import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import ToolSearch from './ToolSearch.svelte';

describe('ToolSearch', () => {
	it('forwards query changes through the shared callback', async () => {
		const onInput = vi.fn();
		render(ToolSearch, { value: 'cpu', onInput, ariaLabel: '筛选进程' } as any);

		const input = screen.getByRole('searchbox', { name: '筛选进程' });
		await fireEvent.input(input, { target: { value: 'chrome' } });
		expect(onInput).toHaveBeenCalledWith('chrome');
	});
});
