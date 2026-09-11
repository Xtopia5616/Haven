import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import GlobalContextMenu from './GlobalContextMenu.svelte';
import { copyText } from '$lib/clipboard.ts';

vi.mock('$lib/clipboard.ts', () => ({
	copyText: vi.fn().mockResolvedValue(true),
}));

afterEach(() => {
	window.getSelection()?.removeAllRanges();
	cleanup();
	vi.clearAllMocks();
});

function selectText(element: HTMLElement) {
	const selection = window.getSelection();
	const range = document.createRange();
	range.selectNodeContents(element);
	selection?.removeAllRanges();
	selection?.addRange(range);
}

describe('GlobalContextMenu', () => {
	it('offers a copy action for selected text without a local menu', async () => {
		const { container } = render(GlobalContextMenu);
		const text = document.createElement('p');
		text.textContent = '工作区中可以复制的文字';
		container.append(text);
		selectText(text);

		const event = new MouseEvent('contextmenu', {
			bubbles: true,
			cancelable: true,
			clientX: 24,
			clientY: 36,
		});
		text.dispatchEvent(event);

		expect(event.defaultPrevented).toBe(true);
		await waitFor(() => expect(screen.getByRole('menu')).toBeTruthy());
		await fireEvent.click(screen.getByText('复制选中内容'));

		expect(copyText).toHaveBeenCalledWith('工作区中可以复制的文字', '选中内容');
	});

	it('leaves a domain-specific context menu in control', async () => {
		const { container } = render(GlobalContextMenu);
		const text = document.createElement('p');
		text.textContent = '由局部菜单处理的文字';
		text.addEventListener('contextmenu', (event) => {
			event.preventDefault();
			event.stopPropagation();
		});
		container.append(text);
		selectText(text);

		const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
		text.dispatchEvent(event);

		expect(event.defaultPrevented).toBe(true);
		expect(container.querySelector('.ctx-menu')).toBeNull();
	});

	it('does not replace the native editing menu for form controls', () => {
		const { container } = render(GlobalContextMenu);
		const text = document.createElement('p');
		text.textContent = '已选择的其他文字';
		container.append(text);
		selectText(text);

		const input = document.createElement('textarea');
		container.append(input);
		const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
		input.dispatchEvent(event);

		expect(event.defaultPrevented).toBe(false);
		expect(container.querySelector('.ctx-menu')).toBeNull();
	});
});
