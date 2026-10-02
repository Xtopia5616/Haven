import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionHeader from './SessionHeader.svelte';

describe('SessionHeader', () => {
	it('renders new, delete, and end actions with the same icon-button geometry', async () => {
		const onNew = vi.fn();
		const onDelete = vi.fn();
		const onEnd = vi.fn();
		render(SessionHeader as any, { onNew, onDelete, onEnd, hasSession: true });
		expect(document.querySelector('.session-header__status')).toBeNull();

		const newButton = screen.getByRole('button', { name: '新建会话' });
		const deleteButton = screen.getByRole('button', { name: '删除会话' });
		const endButton = screen.getByRole('button', { name: '完成会话' });
		for (const button of [newButton, deleteButton, endButton]) {
			expect(button.classList.contains('md-icon-btn')).toBe(true);
			expect(button.getAttribute('data-size')).toBe('toolbar');
		}
		expect(deleteButton.getAttribute('data-variant')).toBe('danger-outline');
		expect(deleteButton.querySelector('[data-icon="delete"]')).toBeTruthy();
		expect(endButton.querySelector('[data-icon="check"]')).toBeTruthy();

		await fireEvent.click(newButton);
		expect(onNew).toHaveBeenCalledTimes(1);
		await fireEvent.click(deleteButton);
		expect(onDelete).toHaveBeenCalledTimes(1);
		await fireEvent.click(endButton);
		expect(onEnd).toHaveBeenCalledTimes(1);
	});

	it('uses the success outline for completing an active conversation', () => {
		render(SessionHeader as any, { hasSession: true });

		expect(screen.getByRole('button', { name: '完成会话' }).getAttribute('data-variant')).toBe(
			'success-outline',
		);
		expect(screen.queryByRole('button', { name: '删除会话' })).toBeTruthy();
	});

	it('hides the delete action when there is no active session', () => {
		render(SessionHeader as any);

		expect(screen.queryByRole('button', { name: '删除会话' })).toBeNull();
	});
});
