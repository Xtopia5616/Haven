import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import AppShell from './AppShell.svelte';

describe('AppShell', () => {
	it('refreshes the application when the Haven title is clicked', async () => {
		const onRefreshApp = vi.fn();
		render(AppShell, { activeTab: 'memory', tabs: [], onRefreshApp } as any);

		await fireEvent.click(screen.getByRole('button', { name: '刷新应用状态' }));

		expect(onRefreshApp).toHaveBeenCalledOnce();
	});
});
