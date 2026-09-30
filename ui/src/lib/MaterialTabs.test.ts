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
		let activeTabOffset = 0;
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
			this: HTMLElement,
		) {
			if (this.classList.contains('md-tabs')) {
				return { left: 100, top: 20, width: 200, height: 56, right: 300, bottom: 76 } as DOMRect;
			}
			return {
				left: 140 + activeTabOffset,
				top: 20,
				width: 80,
				height: 50,
				right: 220 + activeTabOffset,
				bottom: 70,
			} as DOMRect;
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

		await rerender({ tabs, activeTab: 'general', isVisible: false } as any);
		expect(tablist.classList.contains('md-tabs--indicator-ready')).toBe(false);

		activeTabOffset = 40;
		await rerender({ tabs, activeTab: 'general', isVisible: true } as any);
		await waitFor(() => expect(tablist.classList.contains('md-tabs--indicator-ready')).toBe(true));
		expect(tablist.style.getPropertyValue('--md-tab-indicator-x')).toBe('108px');
		expect(tablist.classList.contains('md-tabs--indicator-animating')).toBe(false);
	});

	it('snaps to the measured position initially and animates active-tab changes', async () => {
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
			if (this.id === 'settings-tab-advanced') {
				return { left: 200, top: 20, width: 80, height: 50, right: 280, bottom: 70 } as DOMRect;
			}
			return { left: 120, top: 20, width: 80, height: 50, right: 200, bottom: 70 } as DOMRect;
		});

		const { rerender } = render(MaterialTabs, {
			tabs,
			activeTab: 'general',
			idPrefix: 'settings-tab',
		} as any);
		const tablist = screen.getByRole('tablist');
		const indicator = tablist.querySelector('.md-tabs__indicator')!;

		await waitFor(() =>
			expect(tablist.style.getPropertyValue('--md-tab-indicator-x')).toBe('48px'),
		);
		expect(tablist.classList.contains('md-tabs--indicator-animating')).toBe(false);

		await rerender({ tabs, activeTab: 'advanced', idPrefix: 'settings-tab' } as any);
		await waitFor(() =>
			expect(tablist.style.getPropertyValue('--md-tab-indicator-x')).toBe('128px'),
		);
		expect(tablist.classList.contains('md-tabs--indicator-animating')).toBe(true);

		await fireEvent.transitionEnd(indicator, { propertyName: 'transform' });
		expect(tablist.classList.contains('md-tabs--indicator-animating')).toBe(false);
	});

	it('moves the indicator along the sidebar axis', async () => {
		vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
			callback(0);
			return 1;
		});
		vi.stubGlobal('cancelAnimationFrame', vi.fn());
		vi.stubGlobal('getComputedStyle', () =>
			({
				flexDirection: 'column',
				getPropertyValue: (name: string) =>
					(
						({
							'--md-comp-tab-indicator-height': '3px',
							'--md-comp-tab-indicator-min-width': '24px',
							'--md-sys-space-sm': '8px',
							'--md-sys-space-xl': '20px',
							'--md-comp-tab-indicator-bottom': '3px',
						}) as Record<string, string>
					)[name] ?? '',
			}) as CSSStyleDeclaration,
		);
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
			this: HTMLElement,
		) {
			if (this.classList.contains('md-tabs')) {
				return { left: 100, top: 20, width: 200, height: 180, right: 300, bottom: 200 } as DOMRect;
			}
			if (this.id === 'settings-tab-advanced') {
				return { left: 100, top: 76, width: 200, height: 50, right: 300, bottom: 126 } as DOMRect;
			}
			if (this.id === 'settings-tab-general') {
				return { left: 100, top: 20, width: 200, height: 50, right: 300, bottom: 70 } as DOMRect;
			}
			return { left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 } as DOMRect;
		});

		const view = render(MaterialTabs, {
			tabs,
			activeTab: 'general',
			idPrefix: 'settings-tab',
			className: 'workspace-secondary-tabs--sidebar',
		} as any);
		const tablist = screen.getByRole('tablist');

		await waitFor(() =>
			expect(tablist.style.getPropertyValue('--md-tab-indicator-height')).toBe('20px'),
		);
		expect(tablist.style.getPropertyValue('--md-tab-indicator-width')).toBe('3px');
		expect(tablist.style.getPropertyValue('--md-tab-indicator-y')).toBe('15px');

		await view.rerender({
			tabs,
			activeTab: 'advanced',
			idPrefix: 'settings-tab',
			className: 'workspace-secondary-tabs--sidebar',
		} as any);
		await waitFor(() =>
			expect(tablist.style.getPropertyValue('--md-tab-indicator-y')).toBe('71px'),
		);
		expect(tablist.style.getPropertyValue('--md-tab-indicator-x')).toBe('8px');
		expect(tablist.classList.contains('md-tabs--indicator-animating')).toBe(true);
	});
});
