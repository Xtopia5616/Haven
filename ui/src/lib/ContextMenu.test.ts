import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent, waitFor } from '@testing-library/svelte';
import ContextMenu from './ContextMenu.svelte';

function baseItems() {
	return [
		{ id: 'a', label: '打开', icon: 'open', action: vi.fn() },
		{ id: 'b', label: '删除', icon: 'delete', danger: true, action: vi.fn() },
	];
}

describe('ContextMenu', () => {
	it('renders nothing when closed', () => {
		const { container } = render(ContextMenu, {
			open: false,
			x: 10,
			y: 10,
			items: baseItems(),
		});
		expect(container.querySelector('.ctx-menu')).toBeNull();
	});

	it('renders all items with labels when open', () => {
		const { getByText } = render(ContextMenu, {
			open: true,
			x: 10,
			y: 10,
			items: baseItems(),
		});
		expect(getByText('打开')).toBeTruthy();
		expect(getByText('删除')).toBeTruthy();
	});

	it('calls the item action and closes on click', async () => {
		const items = baseItems();
		const onClose = vi.fn();
		const { getByText } = render(ContextMenu, {
			open: true,
			x: 10,
			y: 10,
			items,
			onClose,
		});
		await fireEvent.click(getByText('打开'));
		expect(items[0].action).toHaveBeenCalledTimes(1);
		expect(onClose).toHaveBeenCalledTimes(1);
	});

	it('marks danger items with the danger class', () => {
		const { getByText } = render(ContextMenu, {
			open: true,
			x: 10,
			y: 10,
			items: baseItems(),
		});
		expect(getByText('删除').closest('button').classList.contains('danger')).toBe(true);
	});

	it('flips the menu when it overflows the right viewport edge', async () => {
		// The fixed menu renders at the cursor first, then flips on measure.
		const { container } = render(ContextMenu, {
			open: true,
			x: window.innerWidth - 1,
			y: 10,
			items: baseItems(),
		});
		const el = /** @type {HTMLElement} */ (container.querySelector('.ctx-menu'));
		await waitFor(() => {
			const left = parseInt(el.style.left, 10);
			expect(left).toBeLessThan(window.innerWidth - el.offsetWidth + 8);
		});
	});

	it('closes on Escape keydown', async () => {
		const onClose = vi.fn();
		render(ContextMenu, { open: true, x: 10, y: 10, items: baseItems(), onClose });
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(onClose).toHaveBeenCalledTimes(1);
	});

	it('closes on outside pointerdown', async () => {
		const onClose = vi.fn();
		render(ContextMenu, { open: true, x: 10, y: 10, items: baseItems(), onClose });
		await fireEvent.pointerDown(document.body, { target: document.body });
		expect(onClose).toHaveBeenCalledTimes(1);
	});

	it('closes on any contextmenu (capture phase)', async () => {
		const onClose = vi.fn();
		render(ContextMenu, { open: true, x: 10, y: 10, items: baseItems(), onClose });
		await fireEvent.contextMenu(document.body);
		expect(onClose).toHaveBeenCalledTimes(1);
	});
});
