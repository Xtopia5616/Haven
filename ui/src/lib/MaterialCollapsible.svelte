<script>
	import Icon from './Icon.svelte';
	import { cubicOut } from 'svelte/easing';
	/**
	 * Material Collapsible — header with a rotating caret.
	 * Same expand/collapse chrome as the settings Limits danger groups.
	 * @prop {boolean} open — bindable; true while expanded
	 * @prop {'default'|'error'} variant
	 * @prop {boolean} lazy — when true, unmount body while collapsed (chat cards)
	 */
	let {
		open = $bindable(false),
		variant = 'default',
		lazy = false,
		header = undefined,
		children = undefined,
	} = $props();

	function toggle() {
		open = !open;
	}

	/** @param {number} duration Keep disclosure motion respectful of the user's system preference. */
	function motionDuration(duration) {
		if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
			return duration;
		}
		return window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : duration;
	}

	/**
	 * Reveal only the block-axis size of the body.
	 *
	 * Svelte's generic slide transition also interpolates padding, margins and
	 * border widths. Tool bodies can contain content-visibility based chat
	 * bubbles, so those extra interpolations may briefly use an intrinsic
	 * placeholder size and then snap back when the transition is released.
	 * Keeping the transition to a measured height and opacity leaves the
	 * inline geometry stable while the body is being revealed.
	 *
	 * @param {HTMLElement} node
	 * @param {{ delay?: number, duration?: number, easing?: (t: number) => number }} options
	 */
	function stableReveal(node, { delay = 0, duration = 400, easing = cubicOut } = {}) {
		const height = node.scrollHeight;
		return {
			delay,
			duration,
			easing,
			css: /** @param {number} t */ (t) =>
				`overflow: hidden; height: ${t * height}px; min-height: 0; opacity: ${Math.min(t * 20, 1)};`,
		};
	}
</script>

<div class="md-collapsible" data-variant={variant} data-open={open} data-lazy={lazy}>
	<button class="md-collapsible-header" type="button" onclick={toggle} aria-expanded={open}>
		<span class="md-collapsible-caret" aria-hidden="true">
			<Icon name="chevronDown" size={12} strokeWidth={2.5} />
		</span>
		<span class="md-collapsible-header-content">
			{@render header?.()}
		</span>
	</button>
	{#if lazy}
		{#if open}
			<div
				class="md-collapsible-body"
				transition:stableReveal={{ duration: motionDuration(240), easing: cubicOut }}
			>
				{@render children?.()}
			</div>
		{/if}
	{:else}
		<div class="md-collapsible-body" hidden={!open}>
			{@render children?.()}
		</div>
	{/if}
</div>

<style>
	.md-collapsible-header {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		padding: 0;
		border: none;
		background: transparent;
		font: inherit;
		color: inherit;
		cursor: pointer;
		text-align: left;
	}
	.md-collapsible-header:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 2px;
		border-radius: 4px;
	}
	.md-collapsible[data-variant='error'] .md-collapsible-header:hover {
		color: var(--md-sys-color-error, #ba1a1a);
	}
	.md-collapsible[data-variant='error'] .md-collapsible-header:focus-visible {
		outline-color: var(--md-sys-color-error, #ba1a1a);
	}
	.md-collapsible-caret {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: transform 0.15s ease;
	}
	.md-collapsible[data-variant='error'] .md-collapsible-caret {
		color: var(--md-sys-color-error, #ba1a1a);
	}
	.md-collapsible-header[aria-expanded='false'] .md-collapsible-caret {
		transform: rotate(-90deg);
	}
	.md-collapsible-header-content {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex: 1;
		min-width: 0;
	}
	.md-collapsible-body {
		display: block;
		width: 100%;
		min-width: 0;
		margin-top: var(--md-sys-space-xs);
	}
</style>
