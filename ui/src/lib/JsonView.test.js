import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import JsonView from './JsonView.svelte';

describe('JsonView', () => {
	it('renders primitives with syntax classes', () => {
		const { container } = render(JsonView, {
			value: { str: 'hi', num: 42, bool: true, nil: null },
		});
		expect(screen.getByText('"str"')).toBeTruthy();
		expect(container.querySelector('.jv-str').textContent).toBe('"hi"');
		expect(container.querySelector('.jv-num').textContent).toBe('42');
		expect(container.querySelector('.jv-bool').textContent).toBe('true');
		expect(container.querySelector('.jv-null').textContent).toBe('null');
	});

	it('renders nested containers collapsed beyond defaultDepth', () => {
		render(JsonView, {
			value: { arr: [1, 2, 3], obj: { k: 'v' } },
			defaultDepth: 1,
		});
		expect(screen.getByText('[ 3 项 ]')).toBeTruthy();
		expect(screen.getByText('{ 1 个键 }')).toBeTruthy();
		expect(screen.queryByText('1')).toBeNull();
		expect(screen.queryByText('"k"')).toBeNull();
	});

	it('renders an empty container with brackets', () => {
		render(JsonView, { value: { empty: [] } });
		expect(screen.getByText('"empty"')).toBeTruthy();
		expect(screen.getByText('[')).toBeTruthy();
		expect(screen.getByText(']')).toBeTruthy();
	});

	it('expands and collapses a nested node on click', async () => {
		const { container } = render(JsonView, {
			value: { arr: [{ x: 1 }] },
			defaultDepth: 1,
		});
		expect(screen.getByText('[ 1 项 ]')).toBeTruthy();

		await fireEvent.click(screen.getByText('[ 1 项 ]'));
		expect(screen.getByText('{ 1 个键 }')).toBeTruthy();

		await fireEvent.click(screen.getByText('{ 1 个键 }'));
		expect(screen.getByText('"x"')).toBeTruthy();
		expect(screen.getByText('1')).toBeTruthy();

		await fireEvent.click(screen.getByText('0'));
		expect(screen.getByText('{ 1 个键 }')).toBeTruthy();
		expect(container.querySelectorAll('.jv-children').length).toBeGreaterThan(0);
	});

	it('copies the raw JSON to the clipboard', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(JsonView, { value: { a: 1, b: [true, null] } });
		await fireEvent.click(screen.getByText('复制'));
		expect(writeText).toHaveBeenCalledWith('{\n  "a": 1,\n  "b": [\n    true,\n    null\n  ]\n}');
		expect(screen.getByText(/已复制/)).toBeTruthy();
	});

	it('does not fail when clipboard is unavailable', async () => {
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: vi.fn().mockRejectedValue(new Error('denied')) },
			configurable: true,
		});
		render(JsonView, { value: { ok: true } });
		await fireEvent.click(screen.getByText('复制'));
		expect(screen.getByText('复制')).toBeTruthy();
	});
});
