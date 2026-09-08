<script>
	import StatusBadge from '$lib/StatusBadge.svelte';
	let { data = {} } = $props();
</script>

<div class="action-row">
	<StatusBadge label={String(data.status)} tone={data.status >= 200 && data.status < 300 ? 'success' : 'error'} />
	{#if data.truncated}<span class="tool-card-meta">（响应过长已截断）</span>{/if}
</div>
{#if typeof data.body === 'string' && data.body}
	<pre class="content-preview">{data.body}</pre>
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
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
</style>
