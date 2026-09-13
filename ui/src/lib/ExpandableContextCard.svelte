<script>
	import { openContextMenu } from '$lib/contextMenu.ts';
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	/**
	 * Shared expandable card shell for resource cards with a context menu.
	 *
	 * @prop {string} cardKind — optional stable marker for card-specific styles
	 * @prop {any[]} contextMenuItems — action objects consumed by the global menu host
	 * @prop {any} header — header snippet
	 * @prop {any} actions — optional header actions snippet
	 * @prop {boolean} showActions — whether to render the optional actions snippet
	 * @prop {any} children — expanded body snippet
	 */
	let {
		cardKind = '',
		contextMenuItems = [],
		header,
		actions = undefined,
		showActions = true,
		children,
	} = $props();
	let expanded = $state(false);

	/** @param {number} duration */
	function motionDuration(duration) {
		if (typeof window === 'undefined' || typeof window.matchMedia !== 'function')
			return duration;
		return window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : duration;
	}

	function toggleExpand() {
		expanded = !expanded;
	}

	/** @param {MouseEvent} event */
	function handleContextMenu(event) {
		openContextMenu(event, contextMenuItems);
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
	class="expandable-context-card motion-list-item"
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
		{#if actions && showActions}
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
		<div
			class="card-body"
			transition:slide={{ duration: motionDuration(180), easing: cubicOut }}
		>
			{@render children?.()}
		</div>
	{/if}
</div>

<style>
	.expandable-context-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		margin: 0;
		overflow: hidden;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.expandable-context-card:hover {
		background: var(--md-sys-color-surface-container);
		border-color: var(--md-sys-color-outline);
		box-shadow: var(--md-sys-elevation-1);
	}
	.expandable-context-card.expanded {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-elevation-1);
	}
	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-md);
		cursor: pointer;
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
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-md);
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
