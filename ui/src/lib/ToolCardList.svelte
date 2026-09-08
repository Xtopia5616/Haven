<script>
	/**
	 * Tool Card List — shared scrollable list surface for structured tool results.
	 * @prop {string} className
	 */
	let { className = '', children } = $props();
</script>

<div class="tool-card-list {className}">
	{@render children?.()}
</div>

<style>
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
</style>
