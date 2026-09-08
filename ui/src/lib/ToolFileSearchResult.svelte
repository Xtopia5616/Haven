<script>
	import ExternalRef from '$lib/ExternalRef.svelte';

	let { data = {} } = $props();
</script>

<div class="tool-card-count">
	{data.count ?? data.results.length} 个结果 · {data.mode === 'content' ? '全文' : '文件名'}
</div>
{#if data.results.length > 0}
	<div class="tool-card-list">
		{#each data.results as result (result.path + (result.line ?? ''))}
			<div class="search-row">
				<ExternalRef class="search-path" target={result.path} />
				{#if result.line != null}
					<span class="search-line">L{result.line}</span>
					<span class="search-snippet">{result.snippet ?? ''}</span>
				{/if}
			</div>
		{/each}
	</div>
{:else}
	<p class="tool-card-empty">没有匹配的结果</p>
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
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
	:global(.search-path) {
		flex: 1;
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-primary);
		text-decoration: underline;
		text-underline-offset: 2px;
		cursor: pointer;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	:global(.search-path:hover) {
		color: color-mix(in srgb, var(--md-sys-color-primary) 80%, var(--md-sys-color-on-surface));
	}
	.search-line {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-secondary);
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
</style>
