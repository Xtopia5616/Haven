import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import MaterialAutocomplete from './MaterialAutocomplete.svelte';

const options = [
	{ value: 'gpt-4o', label: 'GPT-4o' },
	{ value: 'gpt-4o-mini', label: 'GPT-4o Mini' },
	{ value: 'deepseek-chat', label: 'deepseek-chat' },
];

describe('MaterialAutocomplete', () => {
	it('renders the current value', () => {
		render(MaterialAutocomplete, { value: 'gpt-4o', options, onChange: vi.fn() });
		expect(screen.getByDisplayValue('gpt-4o')).toBeTruthy();
	});

	it('shows options on focus and picks one on click', async () => {
		const onChange = vi.fn();
		render(MaterialAutocomplete, { value: '', options, onChange });
		const input = screen.getByRole('combobox');
		await fireEvent.focus(input);
		expect(screen.getByRole('option', { name: /GPT-4o Mini/ })).toBeTruthy();
		await fireEvent.click(screen.getByRole('option', { name: /GPT-4o Mini/ }));
		expect(onChange).toHaveBeenCalledWith('gpt-4o-mini');
		expect(input).toHaveProperty('value', 'gpt-4o-mini');
	});

	it('filters options by typed text and forwards keystrokes', async () => {
		const onChange = vi.fn();
		render(MaterialAutocomplete, { value: '', options, onChange });
		const input = screen.getByRole('combobox');
		await fireEvent.focus(input);
		await fireEvent.input(input, { target: { value: 'deep' } });
		expect(screen.getByRole('option', { name: /deepseek-chat/ })).toBeTruthy();
		expect(screen.queryByRole('option', { name: /GPT-4o/ })).toBeNull();
		expect(onChange).toHaveBeenCalledWith('deep');
	});

	it('does not open a menu without options', async () => {
		render(MaterialAutocomplete, { value: '', options: [], onChange: vi.fn() });
		await fireEvent.focus(screen.getByRole('combobox'));
		expect(screen.queryByRole('listbox')).toBeNull();
	});

	it('calls onFocus when the input is focused', async () => {
		const onFocus = vi.fn();
		render(MaterialAutocomplete, { value: '', options, onChange: vi.fn(), onFocus });
		await fireEvent.focus(screen.getByRole('combobox'));
		expect(onFocus).toHaveBeenCalledTimes(1);
	});

	it('shows a fetching placeholder while loading with no options', async () => {
		render(MaterialAutocomplete, { value: '', options: [], loading: true, onChange: vi.fn() });
		await fireEvent.focus(screen.getByRole('combobox'));
		expect(screen.getByText('Fetching models…')).toBeTruthy();
	});

	it('escape closes the menu', async () => {
		render(MaterialAutocomplete, { value: '', options, onChange: vi.fn() });
		await fireEvent.focus(screen.getByRole('combobox'));
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.queryByRole('listbox')).toBeNull();
	});
});
