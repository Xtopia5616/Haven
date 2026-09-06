import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialNumberField from './MaterialNumberField.svelte';

describe('MaterialNumberField', () => {
	it('supports bounded increase and decrease controls', async () => {
		const onChange = vi.fn();
		render(MaterialNumberField, { value: 3, min: 1, max: 4, onChange });

		const increase = screen.getByRole('button', { name: '增加' });
		const decrease = screen.getByRole('button', { name: '减少' });
		expect(increase.getAttribute('type')).toBe('button');
		expect(decrease.getAttribute('type')).toBe('button');

		await fireEvent.click(increase);
		await fireEvent.click(decrease);

		expect(onChange).toHaveBeenNthCalledWith(1, 4);
		expect(onChange).toHaveBeenNthCalledWith(2, 2);
	});
});
