import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import HotkeyInput from './HotkeyInput.svelte';

describe('HotkeyInput', () => {
	it('shows the current value when idle', () => {
		render(HotkeyInput, { value: 'Ctrl+Shift+Space', onChange: vi.fn() });
		expect(screen.getByText('Ctrl+Shift+Space')).toBeTruthy();
	});

	it('shows the placeholder when empty', () => {
		render(HotkeyInput, { value: '', onChange: vi.fn() });
		expect(screen.getByText('点击并按下快捷键')).toBeTruthy();
	});

	it('enters listening mode on click and emits a formatted combo', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		expect(screen.getByText('按下快捷键组合…')).toBeTruthy();

		await fireEvent.keyDown(window, { key: 'k', code: 'KeyK', ctrlKey: true, shiftKey: true });
		expect(onChange).toHaveBeenCalledWith('Ctrl+Shift+K');
	});

	it('accepts a plain function key without modifiers', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'F5', code: 'F5' });
		expect(onChange).toHaveBeenCalledWith('F5');
	});

	it('formats the Space key and requires a modifier', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: ' ', code: 'Space', ctrlKey: true });
		expect(onChange).toHaveBeenCalledWith('Ctrl+Space');
	});

	it('rejects a plain letter without modifiers', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
		expect(onChange).not.toHaveBeenCalled();
		// Still listening — a valid combo afterwards is accepted.
		await fireEvent.keyDown(window, { key: 'b', code: 'KeyB', ctrlKey: true });
		expect(onChange).toHaveBeenCalledWith('Ctrl+B');
	});

	it('rejects digit keys which the backend does not support', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: '1', code: 'Digit1', ctrlKey: true });
		expect(onChange).not.toHaveBeenCalled();
	});

	it('maps the meta key to Super', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'd', code: 'KeyD', metaKey: true });
		expect(onChange).toHaveBeenCalledWith('Super+D');
	});

	it('escape cancels capture without emitting', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: 'Ctrl+Shift+Space', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' });
		expect(onChange).not.toHaveBeenCalled();
		expect(screen.getByText('Ctrl+Shift+Space')).toBeTruthy();
	});

	it('stops listening after emitting', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'f', code: 'KeyF', ctrlKey: true });
		expect(onChange).toHaveBeenCalledTimes(1);
		// Idle UI restored and a second combo is ignored.
		expect(screen.getByText('点击并按下快捷键')).toBeTruthy();
		await fireEvent.keyDown(window, { key: 'g', code: 'KeyG', ctrlKey: true });
		expect(onChange).toHaveBeenCalledTimes(1);
	});

	it('blur stops listening', async () => {
		const onChange = vi.fn();
		render(HotkeyInput, { value: '', onChange });
		await fireEvent.click(screen.getByRole('button'));
		await fireEvent.blur(screen.getByRole('button'));
		await fireEvent.keyDown(window, { key: 'h', code: 'KeyH', ctrlKey: true });
		expect(onChange).not.toHaveBeenCalled();
	});
});
