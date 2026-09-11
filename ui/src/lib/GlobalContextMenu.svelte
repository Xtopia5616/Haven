<script>
	import { onMount } from 'svelte';
	import ContextMenu from './ContextMenu.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import {
		closeContextMenu,
		contextMenuStore,
		getSelectedText,
		openContextMenu,
	} from '$lib/contextMenu.ts';

	// This is the single rendered menu host. Components submit domain-specific
	// actions through contextMenuStore; the document-level handler below only
	// supplies the default copy action for otherwise unclaimed text selections.
	let contextMenu = $state(
		/** @type {import('$lib/contextMenu.ts').ContextMenuState} */ ({
			open: false,
			x: 0,
			y: 0,
			items: [],
		}),
	);

	const NON_TEXT_CONTEXT_TARGETS =
		'button, input, textarea, select, option, [contenteditable="true"], .ctx-menu';

	/** @param {MouseEvent} event */
	function handleGlobalContextMenu(event) {
		if (event.defaultPrevented) return;
		const target = event.target instanceof Element ? event.target : null;
		if (target?.closest(NON_TEXT_CONTEXT_TARGETS)) return;

		const selectedText = getSelectedText();
		if (!selectedText) return;

		openContextMenu(event, [
			{
				id: 'copy-selection',
				label: '复制选中内容',
				icon: 'copy',
				action: () => copyText(selectedText, '选中内容'),
			},
		]);
	}

	onMount(() => {
		const unsubscribe = contextMenuStore.subscribe((value) => (contextMenu = value));
		// Bubble phase is intentional: a component with a domain-specific menu
		// calls stopPropagation(), so this fallback never replaces that menu.
		document.addEventListener('contextmenu', handleGlobalContextMenu);
		return () => {
			document.removeEventListener('contextmenu', handleGlobalContextMenu);
			unsubscribe();
			closeContextMenu();
		};
	});
</script>

<ContextMenu
	open={contextMenu.open}
	x={contextMenu.x}
	y={contextMenu.y}
	items={contextMenu.items}
	onClose={closeContextMenu}
/>
