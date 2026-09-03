import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialButton from './MaterialButton.svelte';

describe('MaterialButton', () => {
	it('renders shared geometry classes and forwards clicks', async () => {
		const onclick = vi.fn();
		render(MaterialButton, {
			variant: 'outlined' as const,
			label: '新建会话',
			onclick: () => onclick(),
		} as any);

		const button = screen.getByRole('button', { name: '新建会话' });
		expect(button.classList.contains('md-btn')).toBe(true);
		expect(button.classList.contains('md-btn--outlined')).toBe(true);

		await fireEvent.click(button);
		expect(onclick).toHaveBeenCalledTimes(1);
	});
});
