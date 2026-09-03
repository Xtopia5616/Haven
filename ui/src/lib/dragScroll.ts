/**
 * Adds deliberate drag-to-scroll behavior to a scroll container.
 *
 * The resolver variant is used by rendered Markdown, where the scrollable
 * `pre`/`table` elements are created with `{@html}` after the parent mounts.
 */
export type DragScrollAxis = 'x' | 'y';

export type DragScrollOptions = {
	axis?: DragScrollAxis;
	resolveTarget?: (target: EventTarget | null) => HTMLElement | null;
};

type DragState = {
	pointerId: number;
	target: HTMLElement;
	startX: number;
	startY: number;
	startScrollLeft: number;
	startScrollTop: number;
	moved: boolean;
};

const DRAG_THRESHOLD_PX = 4;
const INTERACTIVE_SELECTOR =
	'button, a, input, textarea, select, [role="button"], [contenteditable="true"], pre, table';

function canScroll(node: HTMLElement, axis: DragScrollAxis) {
	return axis === 'x'
		? node.scrollWidth > node.clientWidth + 1
		: node.scrollHeight > node.clientHeight + 1;
}

function startsOnInteractiveElement(target: EventTarget | null) {
	return target instanceof Element && !!target.closest(INTERACTIVE_SELECTOR);
}

/**
 * Create a controller separately from the Svelte action so dynamically
 * rendered descendants can opt into the same drag semantics.
 */
export function createDragScrollController(node: HTMLElement, options: DragScrollOptions = {}) {
	let axis = options.axis ?? 'y';
	let resolveTarget = options.resolveTarget ?? (() => node);
	let drag: DragState | null = null;

	function clearDrag() {
		if (!drag) return;
		drag.target.classList.remove('drag-scroll--active');
		drag = null;
	}

	function onPointerDown(event: PointerEvent) {
		if (event.pointerType === 'mouse' && event.button !== 0) return;
		if (!event.isPrimary) return;
		const target = resolveTarget(event.target);
		if (!target || !canScroll(target, axis)) return;
		// The default page controller starts only on blank space. A delegated
		// controller may intentionally resolve an interactive-looking element
		// such as a horizontally scrollable `pre` or `table`.
		if (target === node && startsOnInteractiveElement(event.target)) return;

		drag = {
			pointerId: event.pointerId,
			target,
			startX: event.clientX,
			startY: event.clientY,
			startScrollLeft: target.scrollLeft,
			startScrollTop: target.scrollTop,
			moved: false,
		};
		node.setPointerCapture?.(event.pointerId);
	}

	function onPointerMove(event: PointerEvent) {
		if (!drag || event.pointerId !== drag.pointerId) return;

		const deltaX = event.clientX - drag.startX;
		const deltaY = event.clientY - drag.startY;
		if (!drag.moved && Math.hypot(deltaX, deltaY) < DRAG_THRESHOLD_PX) return;

		drag.moved = true;
		event.preventDefault();
		drag.target.classList.add('drag-scroll--active');
		if (axis === 'x') {
			drag.target.scrollLeft = drag.startScrollLeft - deltaX;
		} else {
			drag.target.scrollTop = drag.startScrollTop - deltaY;
		}
	}

	function onPointerEnd(event: PointerEvent) {
		if (!drag || event.pointerId !== drag.pointerId) return;
		node.releasePointerCapture?.(event.pointerId);
		clearDrag();
	}

	node.addEventListener('pointerdown', onPointerDown);
	node.addEventListener('pointermove', onPointerMove);
	node.addEventListener('pointerup', onPointerEnd);
	node.addEventListener('pointercancel', onPointerEnd);

	return {
		update(nextOptions: DragScrollOptions = {}) {
			axis = nextOptions.axis ?? 'y';
			resolveTarget = nextOptions.resolveTarget ?? (() => node);
		},
		destroy() {
			clearDrag();
			node.removeEventListener('pointerdown', onPointerDown);
			node.removeEventListener('pointermove', onPointerMove);
			node.removeEventListener('pointerup', onPointerEnd);
			node.removeEventListener('pointercancel', onPointerEnd);
		},
	};
}

/** Svelte action for a vertically scrollable surface. */
export function dragScroll(node: HTMLElement, options: DragScrollOptions = {}) {
	return createDragScrollController(node, options);
}
