import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MenuItem from './MenuItem.svelte';

describe('MenuItem', () => {
	it('shares selection, danger, and click semantics', async () => {
		const onSelect = vi.fn();
		render(MenuItem, { selected: true, danger: true, onSelect, role: 'menuitemradio' } as any);

		const item = screen.getByRole('menuitemradio');
		expect(item.classList.contains('menu-item')).toBe(true);
		expect(item.classList.contains('selected')).toBe(true);
		expect(item.classList.contains('danger')).toBe(true);
		await fireEvent.click(item);
		expect(onSelect).toHaveBeenCalledTimes(1);
	});
});
