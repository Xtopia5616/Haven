import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import AppShell from './AppShell.svelte';

describe('AppShell', () => {
	it('shows a dev badge in the window chrome for development builds', () => {
		render(AppShell, { activeTab: 'memory', tabs: [] } as any);

		if (import.meta.env.DEV) {
			expect(screen.getByText('dev')).toBeTruthy();
		} else {
			expect(screen.queryByText('dev')).toBeNull();
		}
	});

	it('refreshes the application from both responsive Haven title buttons', async () => {
		const onRefreshApp = vi.fn();
		render(AppShell, { activeTab: 'memory', tabs: [], onRefreshApp } as any);
		const refreshButtons = screen.getAllByRole('button', {
			name: '刷新应用状态',
			hidden: true,
		});

		expect(refreshButtons).toHaveLength(2);
		await fireEvent.click(refreshButtons[0]);
		await fireEvent.click(refreshButtons[1]);

		expect(onRefreshApp).toHaveBeenCalledTimes(2);
	});
});
