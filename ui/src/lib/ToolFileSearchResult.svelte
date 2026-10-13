<script lang="ts">
	import ExternalRef from '$lib/ExternalRef.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type { ToolFileSearchMode } from '$lib/toolResultPresentation.ts';

	interface Props {
		data?: {
			results?: Array<{ path: string; line?: number | null; snippet?: string | null }>;
			count?: number | null;
			mode?: ToolFileSearchMode | null;
			search_complete?: boolean | null;
			io_error_count?: number | null;
		};
	}

	let { data = {} }: Props = $props();

	let resultList = $derived(Array.isArray(data.results) ? data.results : []);
	let resultCount = $derived(
		typeof data.count === 'number' && Number.isFinite(data.count)
			? Math.max(0, data.count)
			: resultList.length,
	);
	let searchComplete = $derived(data.search_complete !== false);
	let ioErrorCount = $derived(
		typeof data.io_error_count === 'number' && Number.isInteger(data.io_error_count)
			? Math.max(0, data.io_error_count)
			: 0,
	);
</script>

<div class="tool-result-label">
	{resultCount} 个结果 · {data.mode === 'content' ? '全文' : '文件名'}
</div>
{#if !searchComplete}
	<p class="tool-result-message" role="status">
		搜索未完整完成，当前结果可能不完整。{#if ioErrorCount > 0}有 {ioErrorCount} 次文件或目录读取失败。{/if}
	</p>
{/if}
{#if resultList.length > 0}
	<ToolResultList items={resultList}>
		{#snippet children(visibleResults)}
			<div class="tool-result-scroll-area">
				{#each visibleResults as result (JSON.stringify([result.path, result.line ?? null]))}
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
	{#if searchComplete}
		<p class="tool-result-message">没有匹配的结果</p>
	{/if}
{/if}

<style>
	.search-line {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-secondary);
	}
</style>
