import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import MaterialSwitch from './MaterialSwitch.svelte';

describe('MaterialSwitch', () => {
	const input = () => document.querySelector('.md-switch-input') as HTMLInputElement;

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

	it('forwards the accessible name and disabled state', () => {
		render(MaterialSwitch, { ariaLabel: '切换文件日志', disabled: true } as any);

		expect(input().getAttribute('aria-label')).toBe('切换文件日志');
		expect(input()).toHaveProperty('disabled', true);
	});
});
