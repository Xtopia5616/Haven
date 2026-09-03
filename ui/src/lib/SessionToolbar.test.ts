import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionToolbar from './SessionToolbar.svelte';

describe('SessionToolbar', () => {
	it('uses the shared toolbar icon button for starting a session', async () => {
		const onToggleSessionMenu = vi.fn();
		render(SessionToolbar, { onToggleSessionMenu });

		const button = screen.getByRole('button', { name: '新建会话' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');

		await fireEvent.click(button);
		expect(onToggleSessionMenu).toHaveBeenCalledTimes(1);
	});

	it('uses the shared toolbar icon button for ending a session', () => {
		render(SessionToolbar, { activeSessionId: 'ses-1', messagesLength: 1 });

		const button = screen.getByRole('button', { name: '结束会话' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');
		expect(button.getAttribute('data-variant')).toBe('danger-outline');
	});
});
