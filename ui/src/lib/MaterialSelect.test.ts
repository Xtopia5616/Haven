import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialSelect from './MaterialSelect.svelte';

const options = [
	{ value: 'all', label: '全部类型' },
	{ value: 'background', label: '后台任务' },
];

describe('MaterialSelect', () => {
	it('opens the shared menu and reports the selected value', async () => {
		const onChange = vi.fn();
		render(MaterialSelect, {
			value: 'all',
			options,
			ariaLabel: '任务类型',
			onChange,
		});

		await fireEvent.click(screen.getByRole('combobox', { name: '任务类型' }));
		await fireEvent.click(screen.getByRole('option', { name: '后台任务' }));

		expect(onChange).toHaveBeenCalledWith('background');
		expect(screen.queryByRole('listbox')).toBeNull();
	});

	it('keeps an option click alive when the trigger blurs first', async () => {
		const onChange = vi.fn();
		render(MaterialSelect, { value: 'all', options, onChange });

		const trigger = screen.getByRole('combobox');
		await fireEvent.click(trigger);
		fireEvent.blur(trigger, { relatedTarget: null });
		await fireEvent.click(screen.getByRole('option', { name: '后台任务' }));

		expect(onChange).toHaveBeenCalledWith('background');
	});

	it('supports arrow navigation and selection from the keyboard', async () => {
		const onChange = vi.fn();
		render(MaterialSelect, { value: 'all', options, ariaLabel: '任务类型', onChange });

		const trigger = screen.getByRole('combobox', { name: '任务类型' });
		await fireEvent.keyDown(trigger, { key: 'ArrowDown' });
		expect(trigger.getAttribute('aria-expanded')).toBe('true');
		expect(trigger.getAttribute('aria-activedescendant')).toMatch(/option-0$/);

		await fireEvent.keyDown(trigger, { key: 'ArrowDown' });
		expect(trigger.getAttribute('aria-activedescendant')).toMatch(/option-1$/);
		await fireEvent.keyDown(trigger, { key: 'Home' });
		expect(trigger.getAttribute('aria-activedescendant')).toMatch(/option-0$/);
		await fireEvent.keyDown(trigger, { key: 'End' });
		expect(trigger.getAttribute('aria-activedescendant')).toMatch(/option-1$/);
		await fireEvent.keyDown(trigger, { key: 'Enter' });

		expect(onChange).toHaveBeenCalledWith('background');
		expect(screen.queryByRole('listbox')).toBeNull();
	});

	it('closes the keyboard menu with Escape without changing the selection', async () => {
		const onChange = vi.fn();
		render(MaterialSelect, { value: 'all', options, ariaLabel: '任务类型', onChange });

		const trigger = screen.getByRole('combobox', { name: '任务类型' });
		await fireEvent.keyDown(trigger, { key: 'ArrowDown' });
		await fireEvent.keyDown(trigger, { key: 'Escape' });

		expect(trigger.getAttribute('aria-expanded')).toBe('false');
		expect(onChange).not.toHaveBeenCalled();
	});
});
