<script>
	import MaterialButton from '$lib/MaterialButton.svelte';

	const PAGE_SIZE = 15;
	let { items = [], children } = $props();
	let itemList = $derived(Array.isArray(items) ? items : []);
	let visibleLimit = $state(PAGE_SIZE);
	let visibleItems = $derived(itemList.slice(0, visibleLimit));
	let remainingCount = $derived(Math.max(0, itemList.length - visibleItems.length));

	function showMore() {
		visibleLimit += PAGE_SIZE;
	}
</script>

{@render children?.(visibleItems)}
{#if remainingCount > 0}
	<MaterialButton
		variant="text"
		className="tool-result-more-btn"
		label={`显示更多（剩余 ${remainingCount} 条）`}
		onclick={showMore}
	/>
{/if}

<style>
	:global(.md-btn.tool-result-more-btn) {
		width: 100%;
		box-sizing: border-box;
		margin-top: var(--md-sys-space-xs);
		background: transparent;
		border: 1px dashed var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-primary);
		min-height: var(--md-comp-button-small-height);
	}
	:global(.md-btn.tool-result-more-btn:hover) {
		background: color-mix(in srgb, var(--md-sys-color-primary) 8%, transparent);
	}
</style>
