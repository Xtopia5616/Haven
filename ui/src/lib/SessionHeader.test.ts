import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionHeader from './SessionHeader.svelte';

describe('SessionHeader', () => {
	it('renders the new-session action as a square toolbar icon button', async () => {
		const onNew = vi.fn();
		render(SessionHeader as any, { onNew });

		const button = screen.getByRole('button', { name: '新建会话' });
		expect(button.classList.contains('session-header__new')).toBe(true);
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');

		await fireEvent.click(button);
		expect(onNew).toHaveBeenCalledTimes(1);
	});
});
