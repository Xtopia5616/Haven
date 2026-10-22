<script lang="ts">
	import ExternalRef from '$lib/ExternalRef.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			label?: string | null;
			answer?: string | null;
			queries?: string[];
			filters?: { applied?: string[]; ignored?: string[] };
			fetch_summary?: { requested?: number; fetched?: number; failed?: number };
			results?: Array<{
				url: string;
				title: string;
				snippet?: string | null;
				page_fetch?: {
					status?: string;
					url?: string;
					title?: string;
					http_status?: number;
					format?: string;
					content?: string;
					message?: string;
					truncated?: boolean;
				};
			}>;
		};
	}

	type Citation = NonNullable<NonNullable<Props['data']>['results']>[number];

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

	const filterLabels: Record<string, string> = {
		max_results: '结果数量',
		recency: '时间范围',
		country: '地区',
		language: '语言',
		domains: '域名',
	};
</script>

<div class="tool-result-label">{data.label}</div>
{#if data.answer}
	<p class="tool-result-message">{data.answer}</p>
{/if}
{#if Array.isArray(data.queries) && data.queries.length > 0}
	<div class="tool-result-meta tool-result-meta--compact">查询：{data.queries.join('；')}</div>
{/if}
{#if data.fetch_summary && (data.fetch_summary.fetched || data.fetch_summary.failed)}
	<div class="tool-result-meta tool-result-meta--compact">
		已读取来源页 {data.fetch_summary.fetched ?? 0} 个，失败 {data.fetch_summary.failed ?? 0} 个
	</div>
{/if}
{#if data.filters?.ignored?.length}
	<div class="tool-result-meta tool-result-meta--compact">
		此搜索服务不支持{data.filters.ignored.map((filter) => filterLabels[filter] ?? filter).join('、')}筛选，已忽略。
	</div>
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
						{#if result.page_fetch}
							{#if result.page_fetch.status === 'fetched'}
								<details class="tool-result-page-content">
									<summary>
										已读取来源正文{result.page_fetch.format ? ` · ${result.page_fetch.format}` : ''}{result.page_fetch.truncated ? ' · 已截断' : ''}
									</summary>
									{#if result.page_fetch.title}
										<div class="tool-result-meta tool-result-meta--compact">{result.page_fetch.title}</div>
									{/if}
									<pre>{result.page_fetch.content}</pre>
								</details>
							{:else}
								<div class="tool-result-meta tool-result-meta--compact">
									来源页未读取：{result.page_fetch.message ?? `HTTP ${result.page_fetch.http_status ?? '失败'}`}
								</div>
							{/if}
						{/if}
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else if !data.answer}
		<p class="tool-result-message">（未返回结果）</p>
	{/if}
{/if}

<style>
	.tool-result-page-content {
		margin-top: 0.4rem;
		font-size: 0.82rem;
	}

	.tool-result-page-content summary {
		cursor: pointer;
		color: var(--color-text-secondary, #6b7280);
	}

	.tool-result-page-content pre {
		max-height: 18rem;
		overflow: auto;
		white-space: pre-wrap;
		word-break: break-word;
		margin: 0.4rem 0 0;
		padding: 0.65rem;
		border-radius: 0.4rem;
		background: var(--color-surface-subtle, rgba(127, 127, 127, 0.08));
		font: inherit;
	}
</style>
