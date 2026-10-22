import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import SessionSidebar from './SessionSidebar.svelte';
import { sessionSidebarLayoutStore } from './sessionSidebarLayoutStore.ts';

describe('SessionSidebar', () => {
	beforeEach(() => sessionSidebarLayoutStore.set({ collapsed: false, width: 240 }));

	it('retains layout preferences when returning to the chat workspace', async () => {
		const view = render(SessionSidebar);
		await fireEvent.keyDown(screen.getByRole('separator'), { key: 'ArrowRight' });
		await fireEvent.click(screen.getByRole('button', { name: '折叠会话栏' }));
		view.unmount();
		render(SessionSidebar);
		await fireEvent.click(screen.getByRole('button', { name: '展开会话栏' }));
		expect(screen.getByRole('separator').getAttribute('aria-valuenow')).toBe('256');
	});

	it('preserves width across collapse, keeps new-session access and restores focus', async () => {
		const onNew = vi.fn();
		render(SessionSidebar, { onNew });
		const splitter = screen.getByRole('separator');
		expect(splitter.getAttribute('aria-valuenow')).toBe('240');
		await fireEvent.keyDown(splitter, { key: 'ArrowRight' });
		await fireEvent.click(screen.getByRole('button', { name: '折叠会话栏' }));
		expect(screen.queryByRole('complementary', { name: '会话列表' })).toBeNull();
		expect(document.activeElement).toBe(screen.getByRole('button', { name: '展开会话栏' }));
		await fireEvent.click(screen.getByRole('button', { name: '新建会话' }));
		expect(onNew).toHaveBeenCalledOnce();
		await fireEvent.click(screen.getByRole('button', { name: '展开会话栏' }));
		expect(screen.getByRole('separator').getAttribute('aria-valuenow')).toBe('256');
		expect(document.activeElement).toBe(screen.getByRole('button', { name: '折叠会话栏' }));
	});

	it('limits keyboard resizing and supports resetting to the default', async () => {
		render(SessionSidebar);
		const splitter = screen.getByRole('separator');
		await fireEvent.keyDown(splitter, { key: 'Home' });
		await fireEvent.keyDown(splitter, { key: 'ArrowLeft' });
		expect(splitter.getAttribute('aria-valuenow')).toBe('200');
		await fireEvent.keyDown(splitter, { key: 'End' });
		await fireEvent.keyDown(splitter, { key: 'ArrowRight' });
		expect(splitter.getAttribute('aria-valuenow')).toBe('360');
		await fireEvent.doubleClick(splitter);
		expect(splitter.getAttribute('aria-valuenow')).toBe('240');
	});

	it('resizes by pointer delta and stops on lost pointer capture', async () => {
		render(SessionSidebar);
		const splitter = screen.getByRole('separator');
		Object.defineProperty(splitter, 'setPointerCapture', { value: vi.fn() });
		vi.spyOn(splitter.parentElement!, 'getBoundingClientRect').mockReturnValue({
			width: 240,
		} as DOMRect);
		function pointer(type: string, clientX: number) {
			const event = new Event(type, { bubbles: true });
			Object.assign(event, { button: 0, pointerId: 1, clientX });
			return fireEvent(splitter, event);
		}
		await pointer('pointerdown', 300);
		await pointer('pointermove', 360);
		expect(splitter.getAttribute('aria-valuenow')).toBe('300');
		await pointer('pointermove', 800);
		expect(splitter.getAttribute('aria-valuenow')).toBe('360');
		await pointer('lostpointercapture', 800);
		await pointer('pointermove', 100);
		expect(splitter.getAttribute('aria-valuenow')).toBe('360');
	});
});
