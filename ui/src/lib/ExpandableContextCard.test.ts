import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import ExpandableContextCard from './ExpandableContextCard.svelte';

describe('ExpandableContextCard', () => {
	it('toggles expansion from click and keyboard activation', async () => {
		const { container } = render(ExpandableContextCard);
		const card = container.querySelector('.expandable-context-card') as HTMLElement;
		const header = container.querySelector('.card-header') as HTMLElement;

		expect(card.classList.contains('expanded')).toBe(false);
		expect(header.getAttribute('aria-expanded')).toBe('false');

		await fireEvent.click(header);
		expect(card.classList.contains('expanded')).toBe(true);
		expect(header.getAttribute('aria-expanded')).toBe('true');

		await fireEvent.keyDown(header, { key: ' ' });
		expect(card.classList.contains('expanded')).toBe(false);
	});

	it('opens the shared context menu and stops card activation', async () => {
		const action = vi.fn();
		const { container, getByRole } = render(ExpandableContextCard, {
			contextMenuItems: [{ id: 'copy', label: '复制', action }],
		} as any);
		const card = container.querySelector('.expandable-context-card') as HTMLElement;

		await fireEvent.contextMenu(card, { clientX: 12, clientY: 24 });
		expect(getByRole('menu')).toBeTruthy();
		expect(card.classList.contains('expanded')).toBe(false);

		await fireEvent.click(getByRole('menuitem', { name: '复制' }));
		expect(action).toHaveBeenCalledTimes(1);
	});
});
