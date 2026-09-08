import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialChoiceChip from './MaterialChoiceChip.svelte';

describe('MaterialChoiceChip', () => {
	it('exposes the selected state and selection callback', async () => {
		const onSelect = vi.fn();
		render(MaterialChoiceChip, { label: '自动', selected: true, onSelect } as any);

		const chip = screen.getByRole('button', { name: '自动' });
		expect(chip.getAttribute('aria-pressed')).toBe('true');
		await fireEvent.click(chip);
		expect(onSelect).toHaveBeenCalledTimes(1);
	});
});
