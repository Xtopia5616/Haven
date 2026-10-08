import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import LandscapeWorkspaceNav from './LandscapeWorkspaceNav.svelte';

const tabs = [
	{ id: 'chat', label: '对话' },
	{ id: 'tools', label: '工具' },
];

describe('LandscapeWorkspaceNav', () => {
	afterEach(() => {
		vi.restoreAllMocks();
		vi.unstubAllGlobals();
	});

	it('slides the indicator to the selected workspace', async () => {
		vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
			callback(0);
			return 1;
		});
		vi.stubGlobal('cancelAnimationFrame', vi.fn());
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
			this: HTMLElement,
		) {
			if (this.classList.contains('landscape-workspace-nav')) {
				return {
					left: 100,
					top: 20,
					width: 240,
					height: 112,
					right: 340,
					bottom: 132,
				} as DOMRect;
			}
			if (this.classList.contains('workspace-link')) {
				const top = this.textContent?.includes('工具') ? 78 : 20;
				return {
					left: 100,
					top,
					width: 240,
					height: 54,
					right: 340,
					bottom: top + 54,
				} as DOMRect;
			}
			return { left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 } as DOMRect;
		});

		const onNavigate = vi.fn();
		const view = render(LandscapeWorkspaceNav, { tabs, activeTab: 'chat', onNavigate });
		const nav = screen.getByRole('navigation', { name: '工作区' });
		await waitFor(() =>
			expect(nav.classList.contains('landscape-workspace-nav--indicator-ready')).toBe(true),
		);
		expect(nav.style.getPropertyValue('--workspace-indicator-y')).toBe('17px');

		await fireEvent.click(screen.getByRole('button', { name: '工具' }));
		expect(onNavigate).toHaveBeenCalledWith('tools');
		await view.rerender({ tabs, activeTab: 'tools', onNavigate });
		await waitFor(() =>
			expect(nav.style.getPropertyValue('--workspace-indicator-y')).toBe('75px'),
		);
	});
});
