import { afterEach, describe, expect, it, vi } from 'vitest';
import { createChatViewController } from './chatViewController.ts';

function messagesViewport(overrides: Record<string, unknown> = {}) {
	const bubbles = [
		{ style: { setProperty: vi.fn(), removeProperty: vi.fn() } },
		{ style: { setProperty: vi.fn(), removeProperty: vi.fn() } },
	];
	const list = { querySelectorAll: vi.fn(() => bubbles) };
	const viewport = {
		scrollHeight: 1200,
		scrollTop: 300,
		clientHeight: 400,
		querySelector: vi.fn((selector: string) => selector === '.message-list' ? list : null),
		scrollTo: vi.fn(),
		...overrides,
	};
	return { viewport: viewport as unknown as HTMLElement, bubbles, list };
}

function createHarness() {
	let messages: HTMLElement | null | undefined;
	let autoFollow = true;
	let disposed = false;
	const frames: FrameRequestCallback[] = [];
	const followChanges: boolean[] = [];
	const controller = createChatViewController({
		getMessagesElement: () => messages,
		getAutoFollow: () => autoFollow,
		setAutoFollow: (value) => {
			autoFollow = value;
			followChanges.push(value);
		},
		isDisposed: () => disposed,
		requestFrame: (callback) => {
			frames.push(callback);
			return frames.length;
		},
	});
	return {
		controller,
		frames,
		followChanges,
		setMessages: (value: HTMLElement | null) => (messages = value),
		setAutoFollow: (value: boolean) => (autoFollow = value),
		getAutoFollow: () => autoFollow,
		setDisposed: (value: boolean) => (disposed = value),
	};
}

afterEach(() => {
	vi.useRealTimers();
});

describe('createChatViewController', () => {
	it('coalesces scroll frames and rechecks follow intent before moving the viewport', () => {
		const harness = createHarness();
		const { viewport } = messagesViewport();
		harness.setMessages(viewport);

		harness.controller.scrollToBottom();
		harness.controller.scrollToBottom();
		expect(harness.frames).toHaveLength(1);
		harness.setAutoFollow(false);
		harness.frames.shift()?.(0);
		expect(viewport.scrollTop).toBe(300);

		harness.setAutoFollow(true);
		harness.controller.scrollToBottom();
		harness.frames.shift()?.(16);
		expect(viewport.scrollTop).toBe(1200);
	});

	it('forces two render frames on cold open, then restores lazy bubbles at the real bottom', () => {
		const harness = createHarness();
		const { viewport, bubbles } = messagesViewport();
		harness.setMessages(viewport);

		harness.controller.scrollToBottomAfterOpen();
		expect(viewport.scrollTop).toBe(1200);
		for (const bubble of bubbles) {
			expect(bubble.style.setProperty).toHaveBeenCalledWith('content-visibility', 'visible');
		}
		harness.frames.shift()?.(16);
		expect(harness.frames).toHaveLength(1);
		expect(bubbles[0]?.style.removeProperty).not.toHaveBeenCalled();
		Object.defineProperty(viewport, 'scrollHeight', { value: 1400, configurable: true });
		harness.frames.shift()?.(32);
		expect(viewport.scrollTop).toBe(1400);
		for (const bubble of bubbles) {
			expect(bubble.style.removeProperty).toHaveBeenCalledWith('content-visibility');
		}
	});

	it('keeps follow stable while smooth jump settles and releases it on user input or timeout', async () => {
		vi.useFakeTimers();
		const harness = createHarness();
		const { viewport } = messagesViewport();
		harness.setMessages(viewport);

		harness.controller.jumpToBottom();
		expect(viewport.scrollTo).toHaveBeenCalledWith({ top: 1200, behavior: 'smooth' });
		expect(harness.getAutoFollow()).toBe(true);
		harness.controller.onScroll();
		expect(harness.getAutoFollow()).toBe(true);
		harness.controller.cancelJumpToBottom();
		expect(harness.getAutoFollow()).toBe(false);

		harness.controller.jumpToBottom();
		Object.defineProperty(viewport, 'scrollTop', { value: 800, writable: true, configurable: true });
		await vi.advanceTimersByTimeAsync(700);
		expect(harness.getAutoFollow()).toBe(true);
		expect(harness.followChanges).toEqual([true, false, true, true]);
	});

	it('measures composer clearance, follows resize changes and disconnects the observer', () => {
		const harness = createHarness();
		const { viewport } = messagesViewport();
		harness.setMessages(viewport);
		const style = { setProperty: vi.fn(), removeProperty: vi.fn() };
		const composer = { getBoundingClientRect: () => ({ top: 580 }) };
		const page = {
			querySelector: vi.fn(() => composer),
			getBoundingClientRect: () => ({ bottom: 900 }),
			style,
		} as unknown as HTMLElement;
		const observed: Element[] = [];
		const disconnect = vi.fn();
		let onResize: ResizeObserverCallback | undefined;
		const observer = {
			observe: (element: Element) => observed.push(element),
			disconnect,
		};
		const view = createChatViewController({
			getMessagesElement: () => viewport,
			getAutoFollow: () => true,
			setAutoFollow: vi.fn(),
			isDisposed: () => false,
			requestFrame: vi.fn(() => 1),
			createResizeObserver: (callback) => {
				onResize = callback;
				return observer;
			},
		});

		const cleanup = view.observeComposerClearance(page, true);
		expect(observed).toEqual([composer, page]);
		expect(style.setProperty).toHaveBeenCalledWith('--chat-composer-clearance', '320px');
		expect(harness.frames).toHaveLength(0);
		onResize?.([], observer as unknown as ResizeObserver);
		expect(style.setProperty).toHaveBeenCalledTimes(2);
		cleanup?.();
		expect(disconnect).toHaveBeenCalledOnce();
		expect(style.removeProperty).toHaveBeenCalledWith('--chat-composer-clearance');
	});

	it('ignores scheduled scroll work after disposal', () => {
		const harness = createHarness();
		const { viewport } = messagesViewport();
		harness.setMessages(viewport);
		harness.controller.scrollToBottom();
		harness.setDisposed(true);
		harness.controller.dispose();
		harness.frames.shift()?.(0);
		expect(viewport.scrollTop).toBe(300);
	});
});
