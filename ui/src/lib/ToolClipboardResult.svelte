<script>
	let { data = {} } = $props();
</script>

{#if data.written}
	<p class="tool-card-empty">已写入剪贴板</p>
{:else if Array.isArray(data.entries)}
	{#if data.entries.length > 0}
		<div class="tool-card-list">
			{#each data.entries as entry, index (index)}
				<div class="search-row">
					<span class="search-snippet">{entry.content}</span>
				</div>
			{/each}
		</div>
		<div class="tool-card-meta">共 {data.total} 条历史</div>
	{:else}
		<p class="tool-card-empty">剪贴板历史为空</p>
	{/if}
{:else if typeof data.content === 'string' && data.content}
	<pre class="content-preview">{data.content}</pre>
{:else}
	<p class="tool-card-empty">剪贴板为空</p>
{/if}

<style>
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.tool-card-empty,
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-empty {
		margin: 0;
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		margin-top: var(--md-sys-space-2xs);
	}
	.search-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.search-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.search-snippet {
		flex: none;
		max-width: 140px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
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
