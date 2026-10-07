<script lang="ts">
	import ExternalRef from '$lib/ExternalRef.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			label?: string;
			queries?: string[];
			results?: Array<{ url: string; title: string; snippet?: string }>;
		};
	}

	let { data = {} }: Props = $props();
</script>

<div class="tool-result-label">{data.label}</div>
{#if Array.isArray(data.queries) && data.queries.length > 0}
	<div class="tool-card-meta">查询：{data.queries.join('；')}</div>
{/if}
{#if Array.isArray(data.results)}
	{#if data.results.length > 0}
		<ToolResultList items={data.results}>
			{#snippet children(visibleResults)}
				<div class="tool-result-scroll-area">
					{#each visibleResults as result (result.url + result.title)}
						<div class="tool-result-search-row">
							<ExternalRef class="tool-result-search-path" target={result.url} />
							{#if result.title && result.title !== result.url}
								<span class="tool-result-search-detail">{result.title}</span>
							{/if}
						</div>
						{#if result.snippet}
							<div class="tool-card-meta">{result.snippet}</div>
						{/if}
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">（未返回结果）</p>
	{/if}
{/if}

<style>
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
</style>
