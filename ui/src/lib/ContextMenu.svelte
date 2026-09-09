<script>
	import { tick } from 'svelte';
	import MenuItem from './MenuItem.svelte';
	import Icon from './Icon.svelte';
	import { hasIcon } from './icons.ts';

	// Reusable right-click menu (constraint context_menu_edge_flipping).
	// items: [{ id?, label, icon?, danger?, disabled?, separator?, action? }]
	// icon is a key from the shared icon registry.
	let { open = false, x = 0, y = 0, items = [], onClose = () => {} } = $props();

	let menuEl = /** @type {HTMLDivElement | null} */ ($state(null));
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
		function onPointerDown(/** @type {PointerEvent} */ e) {
			if (menuEl && !menuEl.contains(/** @type {Node | null} */ (e.target))) onClose();
		}
		function onContextMenu() {
			onClose();
		}
		function onKeyDown(/** @type {KeyboardEvent} */ e) {
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

	/** @param {any} item */
	function run(item) {
		item.action?.();
		onClose();
	}
</script>

{#if open}
	<!-- onclick stopPropagation: the menu may live inside a clickable parent
		(e.g. SessionCard), and item clicks must not bubble into it. -->
	<div
		class="ctx-menu motion-menu-enter"
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
				<MenuItem
					className="ctx-item"
					danger={item.danger}
					disabled={item.disabled}
					onSelect={() => run(item)}
				>
					{#snippet children()}
					{#if hasIcon(item.icon)}
						<Icon name={item.icon} size={16} />
					{/if}
					<span class="ctx-label">{item.label}</span>
					{/snippet}
				</MenuItem>
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
