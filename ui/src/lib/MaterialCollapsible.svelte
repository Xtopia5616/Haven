<script>
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
</script>

<div class="md-collapsible" data-variant={variant} data-open={open} data-lazy={lazy}>
	<button class="md-collapsible-header" type="button" onclick={toggle} aria-expanded={open}>
		<span class="md-collapsible-caret" aria-hidden="true">
			<svg
				width="12"
				height="12"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2.5"
				stroke-linecap="round"
				stroke-linejoin="round"
				><polyline points="6 9 12 15 18 9" /></svg
			>
		</span>
		<span class="md-collapsible-header-content">
			{@render header?.()}
		</span>
	</button>
	{#if lazy}
		{#if open}
			<div class="md-collapsible-body">
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
		margin-top: var(--md-sys-space-xs);
	}
</style>
