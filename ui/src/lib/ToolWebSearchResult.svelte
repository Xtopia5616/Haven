<script lang="ts">
	import ExternalRef from '$lib/ExternalRef.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			label?: string | null;
			queries?: string[];
			results?: Array<{ url: string; title: string; snippet?: string | null }>;
		};
	}

	type Citation = { url: string; title: string; snippet?: string | null };

	let { data = {} }: Props = $props();

	function citationKey(results: Citation[], result: Citation, index: number): string {
		const tuple = JSON.stringify([result.url, result.title, result.snippet ?? null]);
		let occurrence = 0;
		for (let prior = 0; prior < index; prior += 1) {
			const candidate = results[prior];
			if (
				JSON.stringify([candidate.url, candidate.title, candidate.snippet ?? null]) === tuple
			) {
				occurrence += 1;
			}
		}
		return JSON.stringify([tuple, occurrence]);
	}
</script>

<div class="tool-result-label">{data.label}</div>
{#if Array.isArray(data.queries) && data.queries.length > 0}
	<div class="tool-result-meta tool-result-meta--compact">查询：{data.queries.join('；')}</div>
{/if}
{#if Array.isArray(data.results)}
	{#if data.results.length > 0}
		<ToolResultList items={data.results}>
			{#snippet children(visibleResults)}
				<div class="tool-result-scroll-area">
					{#each visibleResults as result, index (citationKey(visibleResults, result, index))}
						<div class="tool-result-search-row">
							<ExternalRef class="tool-result-search-path" target={result.url} />
							{#if result.title && result.title !== result.url}
								<span class="tool-result-search-detail">{result.title}</span>
							{/if}
						</div>
						{#if result.snippet}
							<div class="tool-result-meta tool-result-meta--compact">{result.snippet}</div>
						{/if}
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">（未返回结果）</p>
	{/if}
{/if}
