import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import MaterialSwitch from './MaterialSwitch.svelte';

describe('MaterialSwitch', () => {
	const input = () =>
		/** @type {HTMLInputElement} */ (document.querySelector('.md-switch-input'));

	it('renders unchecked by default', () => {
		render(MaterialSwitch, { onChange: vi.fn() });
		expect(input().checked).toBe(false);
	});

	it('reflects the checked prop', () => {
		render(MaterialSwitch, { checked: true, onChange: vi.fn() });
		expect(input().checked).toBe(true);
	});

	it('emits the new checked state on change', async () => {
		const onChange = vi.fn();
		render(MaterialSwitch, { checked: false, onChange });
		await fireEvent.click(input());
		expect(onChange).toHaveBeenCalledWith(true);
	});
});
