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

	it('renders a direct task navigation item with its own active state', async () => {
		const onNavigate = vi.fn();
		render(AppShell, {
			activeTab: 'memory',
			navigationActiveTab: 'tasks',
			tabs: [
				{ id: 'chat', label: '对话', icon: 'chat' },
				{ id: 'memory', label: '历史', icon: 'history' },
			],
			navigationTabs: [
				{ id: 'chat', label: '对话', icon: 'chat' },
				{ id: 'memory', label: '历史', icon: 'history' },
				{ id: 'tasks', label: '任务', icon: 'calendar' },
				{ id: 'long-term-memory', label: '长期记忆', icon: 'memory' },
			],
			onNavigate,
		} as any);

		const taskTabs = screen.getAllByRole('tab', { name: '任务', hidden: true });
		expect(taskTabs.length).toBeGreaterThan(0);
		expect(taskTabs[0].getAttribute('aria-selected')).toBe('true');
		await fireEvent.click(taskTabs[0]);
		expect(onNavigate).toHaveBeenCalledWith('tasks');
		const memoryTabs = screen.getAllByRole('tab', { name: '长期记忆', hidden: true });
		expect(memoryTabs.length).toBeGreaterThan(0);
		await fireEvent.click(memoryTabs[0]);
		expect(onNavigate).toHaveBeenLastCalledWith('long-term-memory');
	});
});
