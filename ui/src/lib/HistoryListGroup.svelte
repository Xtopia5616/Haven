<script lang="ts">
	import type { Snippet } from 'svelte';
	import CountChip from '$lib/CountChip.svelte';

	interface Props {
		id: string;
		title: string;
		count: number;
		children: Snippet;
	}

	let { id, title, count, children }: Props = $props();
</script>

<section class="history-list-group" aria-labelledby={`history-group-${id}`}>
	<div class="history-list-group-header">
		<h3 id={`history-group-${id}`}>{title}</h3>
		<CountChip {count} label="条记录" />
	</div>
	<div class="history-card-grid">
		{@render children()}
	</div>
</section>

<style>
	.history-list-group {
		min-width: 0;
	}
	:global(.history-list-group + .history-list-group) {
		margin-top: var(--md-sys-space-lg);
		padding-top: var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.history-list-group-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin: 0 var(--md-sys-space-xs) var(--md-sys-space-sm);
	}
	.history-list-group-header h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.history-card-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
		align-items: stretch;
		gap: var(--md-sys-space-sm);
		min-width: 0;
		padding: var(--md-sys-space-xs);
	}
</style>
