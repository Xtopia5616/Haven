import { writable } from 'svelte/store';

export type ContextMenuItem = {
	id?: string;
	label?: string;
	icon?: string;
	danger?: boolean;
	disabled?: boolean;
	separator?: boolean;
	action?: () => void | Promise<unknown>;
};

export type ContextMenuState = {
	open: boolean;
	x: number;
	y: number;
	items: ContextMenuItem[];
};

const CLOSED_CONTEXT_MENU: ContextMenuState = {
	open: false,
	x: 0,
	y: 0,
	items: [],
};

/** The one application-wide context-menu request channel. */
export const contextMenuStore = writable<ContextMenuState>(CLOSED_CONTEXT_MENU);

/**
 * Open the shared context menu for a real browser event.
 *
 * The event is stopped after the caller's component has decided that this
 * menu owns the gesture, so the document-level fallback cannot replace it.
 */
export function openContextMenu(event: MouseEvent, items: ContextMenuItem[]): boolean {
	if (items.length === 0) return false;
	event.preventDefault();
	event.stopPropagation();
	return openContextMenuAt(event.clientX, event.clientY, items);
}

/** Open the shared context menu when a caller already owns the event. */
export function openContextMenuAt(x: number, y: number, items: ContextMenuItem[]): boolean {
	if (items.length === 0) return false;
	contextMenuStore.set({ open: true, x, y, items: [...items] });
	return true;
}

export function closeContextMenu() {
	contextMenuStore.set(CLOSED_CONTEXT_MENU);
}

/** Read the current browser text selection, normalized for menu actions. */
export function getSelectedText(): string {
	if (typeof window === 'undefined') return '';
	const selection = window.getSelection();
	if (!selection || selection.isCollapsed) return '';
	return selection.toString().trim();
}

/** Read the selection only when both endpoints belong to a component. */
export function getSelectedTextWithin(element: Element | null): string {
	if (!element) return '';
	if (typeof window === 'undefined') return '';
	const selection = window.getSelection();
	if (
		!selection ||
		selection.isCollapsed ||
		!element.contains(selection.anchorNode) ||
		!element.contains(selection.focusNode)
	) {
		return '';
	}
	return selection.toString().trim();
}
