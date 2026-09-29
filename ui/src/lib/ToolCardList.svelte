<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		className?: string;
		children?: Snippet;
	}

	/**
	 * Tool Card List — shared scrollable list surface for structured tool results.
	 */
	let { className = '', children }: Props = $props();
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
