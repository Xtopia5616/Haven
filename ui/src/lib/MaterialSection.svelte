<script>
	/**
	 * Material Section — page section with title.
	 * @prop {string} title
	 */
	let { title = '', children } = $props();
</script>

<div class="md-section">
	{#if title}
		<h2>{title}</h2>
	{/if}
	{@render children?.()}
</div>

<style>
	.md-section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: 20px;
		margin-bottom: 16px;
	}
	.md-section h2 {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 1px;
		margin-bottom: 16px;
	}
</style>
