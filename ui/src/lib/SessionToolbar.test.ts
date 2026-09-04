import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionToolbar from './SessionToolbar.svelte';

describe('SessionToolbar', () => {
	it('shows the shared toolbar switcher when parallel sessions exist', async () => {
		const onToggleSessionMenu = vi.fn();
		render(SessionToolbar, { showSessionMenu: true, onToggleSessionMenu });

		const button = screen.getByRole('button', { name: '切换会话' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');

		await fireEvent.click(button);
		expect(onToggleSessionMenu).toHaveBeenCalledTimes(1);
	});

	it('does not render duplicate new or end controls in the normal toolbar', () => {
		render(SessionToolbar, { activeSessionId: 'ses-1' });

		expect(screen.queryByRole('button', { name: '新建会话' })).toBeNull();
		expect(screen.queryByRole('button', { name: '结束会话' })).toBeNull();
	});
});
