<script>
	import { onMount } from 'svelte';
	import ContextMenu from './ContextMenu.svelte';
	import { copyText } from '$lib/clipboard.ts';

	// Component-level context menus stop propagation and keep their richer
	// actions. This document-level fallback only handles otherwise unclaimed
	// right-clicks when the user has selected readable text.
	let contextMenu = $state({
		open: false,
		x: 0,
		y: 0,
		selectedText: '',
	});

	const NON_TEXT_CONTEXT_TARGETS =
		'button, input, textarea, select, option, [contenteditable="true"], .ctx-menu';

	function closeContextMenu() {
		contextMenu = { open: false, x: 0, y: 0, selectedText: '' };
	}

	/** @param {MouseEvent} event */
	function handleGlobalContextMenu(event) {
		if (event.defaultPrevented) return;
		const target = event.target instanceof Element ? event.target : null;
		if (target?.closest(NON_TEXT_CONTEXT_TARGETS)) return;

		const selection = window.getSelection();
		const selectedText = selection?.toString().trim() ?? '';
		if (!selectedText) return;

		event.preventDefault();
		contextMenu = {
			open: true,
			x: event.clientX,
			y: event.clientY,
			selectedText,
		};
	}

	async function copySelectedText() {
		await copyText(contextMenu.selectedText, '选中内容');
	}

	let contextMenuItems = $derived([
		{
			id: 'copy-selection',
			label: '复制选中内容',
			icon: 'copy',
			action: copySelectedText,
		},
	]);

	onMount(() => {
		// Bubble phase is intentional: a component with a domain-specific menu
		// calls stopPropagation(), so this fallback never replaces that menu.
		document.addEventListener('contextmenu', handleGlobalContextMenu);
		return () => document.removeEventListener('contextmenu', handleGlobalContextMenu);
	});
</script>

<ContextMenu
	open={contextMenu.open}
	x={contextMenu.x}
	y={contextMenu.y}
	items={contextMenuItems}
	onClose={closeContextMenu}
/>
