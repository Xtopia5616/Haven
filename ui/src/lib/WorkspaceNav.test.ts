import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/svelte';
import WorkspaceNav from './WorkspaceNav.svelte';

const tabs = [
	{ id: 'chat', label: '对话' },
	{ id: 'tools', label: '工具' },
	{ id: 'settings', label: '设置' },
];

describe('WorkspaceNav', () => {
	it('renders the workspace tabs with one selected tab', () => {
		render(WorkspaceNav, { tabs, activeTab: 'tools' });

		const nav = document.querySelector('nav');
		expect(nav?.getAttribute('aria-label')).toBe('工作区导航');
		expect(nav?.querySelector('[role="tablist"]')).not.toBeNull();
		expect(document.querySelectorAll('[role="tab"]')).toHaveLength(3);
		expect(document.querySelector('#workspace-tab-tools')?.getAttribute('aria-selected')).toBe('true');
		expect(document.querySelector('.workspace-nav__indicator')).toBeNull();
		expect(document.querySelector('#workspace-tabpanel-tools')).toBeNull();
	});

	it('delegates navigation without owning route state', async () => {
		const onNavigate = vi.fn();
		const { getByRole } = render(WorkspaceNav, { tabs, onNavigate });

		const toolsTab = getByRole('tab', { name: '工具' });
		await toolsTab.click();
		expect(onNavigate).toHaveBeenCalledWith('tools');
	});
});
