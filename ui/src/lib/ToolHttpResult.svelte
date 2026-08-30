<script>
	let { data = {} } = $props();
</script>

<div class="action-row">
	<span
		class="status-badge status-{data.status >= 200 && data.status < 300
			? 'completed'
			: 'failed'}">{data.status}</span
	>
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
		font-size: 12px;
	}
	.status-badge {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		padding: 1px 8px;
		border-radius: var(--md-sys-shape-full);
	}
	.status-completed {
		background: var(--md-sys-color-success);
		color: var(--md-sys-color-on-success-container);
	}
	.status-failed {
		background: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
	}
	.tool-card-meta {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
</style>
