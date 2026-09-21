import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ModelToolbar from './ModelToolbar.svelte';

describe('ModelToolbar', () => {
	it('shows the active model in the compact switch control', () => {
		render(ModelToolbar, { currentModelName: 'GPT-5', modelMenuOpen: false });

		const button = screen.getByRole('button', { name: '切换默认模型：GPT-5' });
		expect(button.textContent).toContain('GPT-5');
		expect(button.getAttribute('aria-haspopup')).toBe('menu');
		expect(button.getAttribute('aria-expanded')).toBe('false');
	});
});
