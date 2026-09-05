import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import RefreshButton from './RefreshButton.svelte';

describe('RefreshButton', () => {
	it('shares loading semantics and keeps the compact geometry class', () => {
		render(RefreshButton, { compact: true, loading: true, onclick: vi.fn() } as any);

		const button = screen.getByRole('button', { name: '刷新中…' });
		expect(button.classList.contains('refresh-button')).toBe(true);
		expect(button.classList.contains('refresh-button--compact')).toBe(true);
		expect(button.classList.contains('refresh-button--loading')).toBe(true);
		expect(button).toHaveProperty('disabled', true);
		expect(button.getAttribute('aria-busy')).toBe('true');
	});

	it('forwards the refresh callback when idle', async () => {
		const onclick = vi.fn();
		render(RefreshButton, { label: '刷新模型列表', onclick } as any);

		await fireEvent.click(screen.getByRole('button', { name: '刷新模型列表' }));
		expect(onclick).toHaveBeenCalledTimes(1);
	});

	it('renders compact refresh actions as a shared icon button', () => {
		render(RefreshButton, { compact: true, iconOnly: true, title: '刷新 MCP 连接' } as any);

		const button = screen.getByRole('button', { name: '刷新' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.classList.contains('refresh-button--compact')).toBe(true);
		expect(button.querySelector('svg')).toBeTruthy();
		expect(button.getAttribute('title')).toBe('刷新 MCP 连接');
	});
});
