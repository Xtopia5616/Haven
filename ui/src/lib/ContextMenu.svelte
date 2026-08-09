<script module>
	// Built-in icon set (inner SVG markup) for context menu items. Items
	// reference an icon by name from this map, or pass raw SVG inner markup
	// directly for a custom glyph. Kept central so every context menu shares
	// the same visual language.
	export const ICONS = {
		copy: '<rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
		rollback:
			'<polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/>',
		branch:
			'<circle cx="6" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M6 9v6"/><path d="M18 9h-6a4 4 0 0 0-4 4v4"/><circle cx="18" cy="6" r="3"/>',
		delete:
			'<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>',
		edit: '<path d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"/>',
		open: '<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
		export:
			'<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>',
		refresh:
			'<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>',
		play: '<polygon points="5 3 19 12 5 21 5 3"/>',
		pause: '<rect x="6" y="4" width="4" height="16"/><rect x="14" y="4" width="4" height="16"/>',
		stop: '<rect x="4" y="4" width="16" height="16" rx="2"/>',
		power: '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>',
	};
</script>

<script>
	import { tick } from 'svelte';

	// Reusable right-click menu (constraint context_menu_edge_flipping).
	// items: [{ id?, label, icon?, danger?, disabled?, separator?, action? }]
	// icon is a key from the ICONS map above or raw SVG inner markup.
	let { open = false, x = 0, y = 0, items = [], onClose = () => {} } = $props();

	let menuEl = $state(null);
	let pos = $state({ x: 0, y: 0 });

	// Flip to the other side of the cursor when the menu would overflow the
	// viewport edge, so it never renders off-screen.
	$effect(() => {
		if (!open) return;
		pos = { x, y };
		tick().then(() => {
			if (!menuEl) return;
			const rect = menuEl.getBoundingClientRect();
			const vw = window.innerWidth;
			const vh = window.innerHeight;
			let nx = pos.x;
			let ny = pos.y;
			if (nx + rect.width > vw - 8) nx = Math.max(8, nx - rect.width);
			if (ny + rect.height > vh - 8) ny = Math.max(8, ny - rect.height);
			if (nx !== pos.x || ny !== pos.y) pos = { x: nx, y: ny };
		});
	});

	// Outside click / right-click / Escape dismisses the menu. The right-click
	// listener runs in the CAPTURE phase so it fires before any other
	// element's own contextmenu handler: when a new context menu opens from
	// anywhere (e.g. a nested card), this one closes first instead of staying
	// stacked under it.
	$effect(() => {
		if (!open) return;
		function onPointerDown(e) {
			if (menuEl && !menuEl.contains(e.target)) onClose();
		}
		function onContextMenu() {
			onClose();
		}
		function onKeyDown(e) {
			if (e.key === 'Escape') onClose();
		}
		window.addEventListener('pointerdown', onPointerDown);
		window.addEventListener('contextmenu', onContextMenu, true);
		window.addEventListener('keydown', onKeyDown);
		return () => {
			window.removeEventListener('pointerdown', onPointerDown);
			window.removeEventListener('contextmenu', onContextMenu, true);
			window.removeEventListener('keydown', onKeyDown);
		};
	});

	function iconMarkup(name) {
		// Icons resolve strictly against the built-in set; unknown names are
		// ignored so no caller-supplied string can reach the raw-HTML sink.
		return ICONS[name] ?? '';
	}

	function run(item) {
		item.action?.();
		onClose();
	}
</script>

{#if open}
	<!-- onclick stopPropagation: the menu may live inside a clickable parent
		(e.g. TaskCard), and item clicks must not bubble into it. -->
	<div
		class="ctx-menu"
		bind:this={menuEl}
		style="left: {pos.x}px; top: {pos.y}px;"
		role="menu"
		tabindex="0"
		onclick={(e) => e.stopPropagation()}
		onkeydown={(e) => e.stopPropagation()}
	>
		{#each items as item (item.id ?? item.label)}
			{#if item.separator}
				<div class="ctx-sep" role="separator"></div>
			{:else}
				<button
					class="ctx-item"
					class:danger={item.danger}
					disabled={item.disabled}
					role="menuitem"
					type="button"
					onclick={() => run(item)}
				>
					{#if iconMarkup(item.icon)}
						<svg
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							>{@html iconMarkup(item.icon)}</svg
						>
					{/if}
					<span class="ctx-label">{item.label}</span>
				</button>
			{/if}
		{/each}
	</div>
{/if}

<style>
	.ctx-menu {
		position: fixed;
		z-index: 1000;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
		min-width: 160px;
		display: flex;
		flex-direction: column;
	}
	.ctx-item {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition:
			background var(--md-sys-motion-duration-fast)
				var(--md-sys-motion-easing-standard);
	}
	.ctx-item:hover:not(:disabled) {
		background: var(--md-sys-color-surface-container-highest);
	}
	.ctx-item:disabled {
		opacity: 0.38;
		cursor: not-allowed;
	}
	.ctx-item.danger {
		color: var(--md-sys-color-error);
	}
	.ctx-item.danger:hover:not(:disabled) {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.ctx-item svg {
		flex-shrink: 0;
	}
	.ctx-label {
		flex: 1;
		text-align: left;
	}
	.ctx-sep {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-2xs) var(--md-sys-space-sm);
	}
</style>
