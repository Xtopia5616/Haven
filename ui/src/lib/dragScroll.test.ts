import { describe, expect, it } from 'vitest';
import { dragScroll } from './dragScroll.ts';

function sizedScrollableNode({
	scrollHeight = 1000,
	clientHeight = 400,
	scrollWidth = 0,
	clientWidth = 0,
} = {}) {
	const node = document.createElement('div');
	Object.defineProperties(node, {
		scrollHeight: { configurable: true, value: scrollHeight, writable: true },
		clientHeight: { configurable: true, value: clientHeight },
		scrollWidth: { configurable: true, value: scrollWidth, writable: true },
		clientWidth: { configurable: true, value: clientWidth },
		scrollTop: { configurable: true, value: 0, writable: true },
		scrollLeft: { configurable: true, value: 0, writable: true },
	});
	return node;
}

describe('dragScroll', () => {
	it('scrolls vertically when dragging the empty surface', () => {
		const node = sizedScrollableNode();
		const cleanup = dragScroll(node, { axis: 'y' });

		node.dispatchEvent(
			new PointerEvent('pointerdown', {
				bubbles: true,
				button: 0,
				clientX: 100,
				clientY: 300,
				isPrimary: true,
				pointerId: 1,
			}),
		);
		node.dispatchEvent(
			new PointerEvent('pointermove', {
				bubbles: true,
				button: 0,
				clientX: 100,
				clientY: 180,
				isPrimary: true,
				pointerId: 1,
			}),
		);

		expect(node.scrollTop).toBe(120);
		cleanup.destroy();
	});

	it('locks the drag to the configured vertical axis', () => {
		const node = sizedScrollableNode();
		const cleanup = dragScroll(node, { axis: 'y' });

		node.dispatchEvent(
			new PointerEvent('pointerdown', {
				bubbles: true,
				button: 0,
				clientX: 100,
				clientY: 300,
				isPrimary: true,
				pointerId: 1,
			}),
		);
		node.dispatchEvent(
			new PointerEvent('pointermove', {
				bubbles: true,
				button: 0,
				clientX: 220,
				clientY: 240,
				isPrimary: true,
				pointerId: 1,
			}),
		);

		expect(node.scrollTop).toBe(60);
		cleanup.destroy();
	});

	it('does not start a page drag from an interactive child', () => {
		const node = sizedScrollableNode();
		const button = document.createElement('button');
		node.append(button);
		const cleanup = dragScroll(node, { axis: 'y' });

		button.dispatchEvent(
			new PointerEvent('pointerdown', {
				bubbles: true,
				button: 0,
				clientX: 100,
				clientY: 300,
				isPrimary: true,
				pointerId: 1,
			}),
		);
		node.dispatchEvent(
			new PointerEvent('pointermove', {
				bubbles: true,
				button: 0,
				clientX: 100,
				clientY: 180,
				isPrimary: true,
				pointerId: 1,
			}),
		);

		expect(node.scrollTop).toBe(0);
		cleanup.destroy();
	});

	it('supports horizontal dragging for a resolved code or table surface', () => {
		const node = document.createElement('div');
		const code = sizedScrollableNode({
			scrollHeight: 400,
			clientHeight: 400,
			scrollWidth: 1000,
			clientWidth: 400,
		});
		node.append(code);
		const cleanup = dragScroll(node, {
			axis: 'x',
			resolveTarget: (target) => (target instanceof HTMLElement ? target : null),
		});

		code.dispatchEvent(
			new PointerEvent('pointerdown', {
				bubbles: true,
				button: 0,
				clientX: 300,
				clientY: 100,
				isPrimary: true,
				pointerId: 1,
			}),
		);
		code.dispatchEvent(
			new PointerEvent('pointermove', {
				bubbles: true,
				button: 0,
				clientX: 180,
				clientY: 100,
				isPrimary: true,
				pointerId: 1,
			}),
		);

		expect(code.scrollLeft).toBe(120);
		cleanup.destroy();
	});
});
