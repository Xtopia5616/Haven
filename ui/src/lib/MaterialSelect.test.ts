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

		await fireEvent.click(screen.getByRole('button', { name: '任务类型' }));
		await fireEvent.click(screen.getByRole('option', { name: '后台任务' }));

		expect(onChange).toHaveBeenCalledWith('background');
		expect(screen.queryByRole('listbox')).toBeNull();
	});

	it('keeps an option click alive when the trigger blurs first', async () => {
		const onChange = vi.fn();
		render(MaterialSelect, { value: 'all', options, onChange });

		const trigger = screen.getByRole('button');
		await fireEvent.click(trigger);
		fireEvent.blur(trigger, { relatedTarget: null });
		await fireEvent.click(screen.getByRole('option', { name: '后台任务' }));

		expect(onChange).toHaveBeenCalledWith('background');
	});
});
