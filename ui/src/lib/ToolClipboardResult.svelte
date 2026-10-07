<script lang="ts">
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			written?: boolean;
			entries?: Array<{ content: string }>;
			total?: number;
			content?: string;
		};
	}

	let { data = {} }: Props = $props();
</script>

{#if data.written}
	<p class="tool-result-message">已写入剪贴板</p>
{:else if Array.isArray(data.entries)}
	{#if data.entries.length > 0}
		<ToolResultList items={data.entries}>
			{#snippet children(visibleEntries)}
				<div class="tool-result-scroll-area">
					{#each visibleEntries as entry, index (index)}
						<div class="tool-result-search-row">
							<span class="tool-result-search-detail">{entry.content}</span>
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
		<div class="tool-card-meta">共 {data.total} 条历史</div>
	{:else}
		<p class="tool-result-message">剪贴板历史为空</p>
	{/if}
{:else if typeof data.content === 'string' && data.content}
	<pre class="tool-result-preview">{data.content}</pre>
{:else}
	<p class="tool-result-message">剪贴板为空</p>
{/if}

<style>
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		margin-top: var(--md-sys-space-2xs);
	}
</style>
