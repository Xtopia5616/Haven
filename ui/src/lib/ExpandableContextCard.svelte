<script>
	import ContextMenu from '$lib/ContextMenu.svelte';

	/**
	 * Shared expandable card shell for resource cards with a context menu.
	 *
	 * @prop {string} cardKind — optional stable marker for card-specific styles
	 * @prop {any[]} contextMenuItems — action objects consumed by ContextMenu
	 * @prop {any} header — header snippet
	 * @prop {any} actions — optional header actions snippet
	 * @prop {any} children — expanded body snippet
	 */
	let { cardKind = '', contextMenuItems = [], header, actions, children } = $props();
	let expanded = $state(false);
	let contextMenu = $state({ open: false, x: 0, y: 0 });

	function toggleExpand() {
		expanded = !expanded;
	}

	/** @param {MouseEvent} event */
	function handleContextMenu(event) {
		event.preventDefault();
		event.stopPropagation();
		contextMenu = { open: true, x: event.clientX, y: event.clientY };
	}

	function closeContextMenu() {
		contextMenu = { open: false, x: 0, y: 0 };
	}

	/** @param {KeyboardEvent} event */
	function handleHeaderKeydown(event) {
		if (event.key !== 'Enter' && event.key !== ' ') return;
		event.preventDefault();
		toggleExpand();
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="expandable-context-card"
	data-card-kind={cardKind || undefined}
	class:expanded
	oncontextmenu={handleContextMenu}
>
	<div
		class="card-header"
		onclick={toggleExpand}
		onkeydown={handleHeaderKeydown}
		role="button"
		tabindex="0"
		aria-expanded={expanded}
	>
		<div class="card-info">
			{@render header?.()}
		</div>
		{#if actions}
			<div
				class="card-actions"
				onclick={(event) => event.stopPropagation()}
				onkeydown={(event) => event.stopPropagation()}
				role="presentation"
			>
				{@render actions()}
			</div>
		{/if}
	</div>
	{#if expanded}
		<div class="card-body">
			{@render children?.()}
		</div>
	{/if}

	<ContextMenu
		open={contextMenu.open}
		x={contextMenu.x}
		y={contextMenu.y}
		items={contextMenuItems}
		onClose={closeContextMenu}
	/>
</div>

<style>
	.expandable-context-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		margin-bottom: var(--md-sys-space-sm);
		overflow: hidden;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.expandable-context-card:hover {
		border-color: var(--md-sys-color-outline);
	}
	.expandable-context-card.expanded {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-elevation-1);
	}
	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
		cursor: pointer;
		user-select: none;
	}
	.card-info {
		flex: 1;
		min-width: 0;
	}
	.card-actions {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
		flex-shrink: 0;
		margin-left: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-2xs);
	}
	.card-body {
		padding: 0 var(--md-sys-space-xl) var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	:global(.expandable-context-card .card-body h4) {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		margin: var(--md-sys-space-md) 0 var(--md-sys-space-sm);
		font-weight: 700;
	}
</style>
