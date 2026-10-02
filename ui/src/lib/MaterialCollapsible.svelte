<script lang="ts">
	import Icon from './Icon.svelte';
	import { cubicOut } from 'svelte/easing';
	import type { Snippet } from 'svelte';

	interface Props {
		open?: boolean;
		variant?: 'default' | 'error';
		lazy?: boolean;
		header?: Snippet;
		children?: Snippet;
	}

	/**
	 * Material Collapsible — header with a rotating caret.
	 * Same expand/collapse chrome as the settings Limits danger groups.
	 */
	let {
		open = $bindable(false),
		variant = 'default',
		lazy = false,
		header = undefined,
		children = undefined,
	}: Props = $props();

	function toggle(): void {
		open = !open;
	}

	/** Animate the intrinsic grid track so live content changes do not snap to
	 * a height measured only once at the start of the transition. */
	function stableReveal(
		node: HTMLElement,
		{
			delay = 0,
			duration = 240,
			easing = cubicOut,
		}: { delay?: number; duration?: number; easing?: (t: number) => number } = {},
	) {
		const marginTop = Number.parseFloat(getComputedStyle(node).marginTop) || 0;
		return {
			delay,
			duration,
			easing,
			css: (t: number) =>
				`grid-template-rows: minmax(0, ${t}fr); margin-top: ${t * marginTop}px; opacity: ${Math.min(t * 20, 1)};`,
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
				transition:stableReveal
			>
				<div class="md-collapsible-body-content">
					{@render children?.()}
				</div>
			</div>
		{/if}
	{:else}
		<div
			class="md-collapsible-body md-collapsible-body--retained"
			aria-hidden={!open}
			inert={!open}
		>
			<div class="md-collapsible-body-content">
				{@render children?.()}
			</div>
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
		display: grid;
		grid-template-rows: minmax(0, 1fr);
		width: 100%;
		min-width: 0;
		margin-top: var(--md-sys-space-xs);
		overflow-anchor: none;
	}
	.md-collapsible-body-content {
		min-height: 0;
		overflow: hidden;
	}
	.md-collapsible-body--retained {
		grid-template-rows: minmax(0, 0fr);
		margin-top: 0;
		opacity: 0;
		pointer-events: none;
		transition:
			grid-template-rows 240ms var(--md-sys-motion-easing-emphasized),
			margin-top 240ms var(--md-sys-motion-easing-emphasized),
			opacity 120ms linear;
	}
	.md-collapsible[data-open='true'] > .md-collapsible-body--retained {
		grid-template-rows: minmax(0, 1fr);
		margin-top: var(--md-sys-space-xs);
		opacity: 1;
		pointer-events: auto;
	}
	@media (prefers-reduced-motion: reduce) {
		.md-collapsible-body--retained {
			transition: none;
		}
	}
</style>
