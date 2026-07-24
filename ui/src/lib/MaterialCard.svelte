<script>
	/**
	 * Material Card — standardised card container.
	 * @prop {'elevated'|'outlined'|'filled'} variant
	 */
	let { variant = 'filled', children } = $props();
</script>

<div class="md-card" data-variant={variant}>
	{@render children?.()}
</div>

<style>
	.md-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-lg);
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-card[data-variant='elevated'] {
		box-shadow: var(--md-sys-elevation-1);
	}
	.md-card[data-variant='elevated']:hover {
		box-shadow: var(--md-sys-elevation-2);
	}
	.md-card[data-variant='outlined'] {
		background: transparent;
	}
</style>
