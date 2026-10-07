<script lang="ts">
	import StatusBadge from '$lib/StatusBadge.svelte';

	interface Props {
		data?: {
			status?: number;
			truncated?: boolean;
			body?: unknown;
		};
	}

	let { data = {} }: Props = $props();
</script>

<div class="action-row">
	<StatusBadge
		label={String(data.status)}
		tone={Number(data.status) >= 200 && Number(data.status) < 300 ? 'success' : 'error'}
		className={Number(data.status) >= 200 && Number(data.status) < 300
			? 'status-completed'
			: 'status-failed'}
	/>
	{#if data.truncated}<span class="tool-card-meta">（响应过长已截断）</span>{/if}
</div>
{#if typeof data.body === 'string' && data.body}
	<pre class="tool-result-preview">{data.body}</pre>
{/if}

<style>
	.action-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
</style>
