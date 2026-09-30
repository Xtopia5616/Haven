import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import MaterialTabs from './MaterialTabs.svelte';

const tabs = [
	{ id: 'general', label: '通用', hint: '基础设置' },
	{ id: 'advanced', label: '高级' },
];

describe('MaterialTabs', () => {
	afterEach(() => {
		vi.restoreAllMocks();
		vi.unstubAllGlobals();
	});

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

	it('remeasures the indicator when a retained workspace becomes visible again', async () => {
		vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
			callback(0);
			return 1;
		});
		vi.stubGlobal('cancelAnimationFrame', vi.fn());
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
			this: HTMLElement,
		) {
			if (this.classList.contains('md-tabs')) {
				return { left: 100, top: 20, width: 200, height: 56, right: 300, bottom: 76 } as DOMRect;
			}
			return { left: 140, top: 20, width: 80, height: 50, right: 220, bottom: 70 } as DOMRect;
		});

		const { rerender } = render(MaterialTabs, {
			tabs,
			activeTab: 'general',
			isVisible: false,
		} as any);
		const tablist = screen.getByRole('tablist');
		expect(tablist.classList.contains('md-tabs--indicator-ready')).toBe(false);

		await rerender({ tabs, activeTab: 'general', isVisible: true } as any);
		await waitFor(() => expect(tablist.classList.contains('md-tabs--indicator-ready')).toBe(true));
		expect(tablist.style.getPropertyValue('--md-tab-indicator-x')).toBe('68px');
	});
});
