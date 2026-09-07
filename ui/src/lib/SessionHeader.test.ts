import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionHeader from './SessionHeader.svelte';

describe('SessionHeader', () => {
	it('renders new and end actions with the same icon-button geometry', async () => {
		const onNew = vi.fn();
		const onEnd = vi.fn();
		render(SessionHeader as any, { onNew, onEnd, hasSession: true });

		const newButton = screen.getByRole('button', { name: '新建会话' });
		const endButton = screen.getByRole('button', { name: '结束会话' });
		for (const button of [newButton, endButton]) {
			expect(button.classList.contains('md-icon-btn')).toBe(true);
			expect(button.getAttribute('data-size')).toBe('toolbar');
		}

		await fireEvent.click(newButton);
		expect(onNew).toHaveBeenCalledTimes(1);
		await fireEvent.click(endButton);
		expect(onEnd).toHaveBeenCalledTimes(1);
	});
});
