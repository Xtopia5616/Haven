<script lang="ts">
	import ExternalRef from '$lib/ExternalRef.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			results?: Array<{ path: string; line?: number | null; snippet?: string }>;
			count?: number;
			mode?: string;
		};
	}

	let { data = {} }: Props = $props();

	let resultList = $derived(Array.isArray(data.results) ? data.results : []);
	let resultCount = $derived(
		typeof data.count === 'number' && Number.isFinite(data.count)
			? Math.max(0, data.count)
			: resultList.length,
	);
</script>

<div class="tool-result-label">
	{resultCount} 个结果 · {data.mode === 'content' ? '全文' : '文件名'}
</div>
{#if resultList.length > 0}
	<ToolResultList items={resultList}>
		{#snippet children(visibleResults)}
			<div class="tool-result-scroll-area">
				{#each visibleResults as result (result.path + (result.line ?? ''))}
					<div class="tool-result-search-row">
						<ExternalRef class="tool-result-search-path" target={result.path} />
						{#if result.line != null}
							<span class="search-line">L{result.line}</span>
							<span class="tool-result-search-detail">{result.snippet ?? ''}</span>
						{/if}
					</div>
				{/each}
			</div>
		{/snippet}
	</ToolResultList>
{:else}
	<p class="tool-card-empty">没有匹配的结果</p>
{/if}

<style>
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.search-line {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-secondary);
	}
</style>
