import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialTabs from './MaterialTabs.svelte';

const tabs = [
	{ id: 'general', label: '通用', hint: '基础设置' },
	{ id: 'advanced', label: '高级' },
];

describe('MaterialTabs', () => {
	it('renders one shared tablist with selection semantics', () => {
		render(MaterialTabs, { tabs, activeTab: 'general', panelId: 'settings-panel' } as any);

		expect(screen.getByRole('tablist', { name: '页签' })).toBeTruthy();
		expect(screen.getByRole('tab', { name: /通用 基础设置/ }).getAttribute('aria-selected')).toBe('true');
		expect(screen.getByRole('tab', { name: '高级' }).getAttribute('aria-controls')).toBe('settings-panel');
	});

	it('delegates tab changes', async () => {
		const onNavigate = vi.fn();
		render(MaterialTabs, { tabs, activeTab: 'general', onNavigate } as any);

		await fireEvent.click(screen.getByRole('tab', { name: '高级' }));
		expect(onNavigate).toHaveBeenCalledWith('advanced');
	});
});
