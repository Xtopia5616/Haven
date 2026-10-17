<script lang="ts">
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import type { ModelDraft, ProviderDraft } from '$lib/settingsModelTypes.ts';

	interface Props {
		model: ModelDraft;
		provider?: ProviderDraft;
	}

	const componentId = $props.id();
	let { model, provider }: Props = $props();
	const fieldId = `independent-search-${componentId}`;
	const providerId = $derived(provider?.provider?.trim().toLowerCase() ?? '');
	function setEnabled(enabled: boolean) {
		if (!enabled) {
			model.independent_search = null;
			return;
		}
		switch (providerId) {
			case 'deepseek':
				model.independent_search = {
					provider: 'deepseek_search',
					count: 10,
					need_content: false,
				};
				break;
			case 'perplexity':
				model.independent_search = {
					provider: 'perplexity_search',
					max_results: 10,
					search_type: 'web',
					recency_filter: null,
				};
				break;
			case 'mistral':
				model.independent_search = { provider: 'mistral_web_search', premium: false };
				break;
			case 'zhipu':
				model.independent_search = { provider: 'zhipu_web_search' };
				break;
		}
	}

	function updateDeepSeekCount(count: number) {
		const config = model.independent_search;
		if (config?.provider === 'deepseek_search') {
			model.independent_search = { ...config, count };
		}
	}

	function updateDeepSeekContent(needContent: boolean) {
		const config = model.independent_search;
		if (config?.provider === 'deepseek_search') {
			model.independent_search = { ...config, need_content: needContent };
		}
	}

	function updatePerplexityResults(maxResults: number) {
		const config = model.independent_search;
		if (config?.provider === 'perplexity_search') {
			model.independent_search = { ...config, max_results: maxResults };
		}
	}

	function updatePerplexityType(searchType: string) {
		const config = model.independent_search;
		if (config?.provider === 'perplexity_search') {
			const currentMaxResults = config.max_results ?? 10;
			const maxResults =
				searchType === 'people' ? currentMaxResults : Math.min(currentMaxResults, 20);
			model.independent_search = {
				...config,
				search_type: searchType as typeof config.search_type,
				max_results: maxResults,
			};
		}
	}

	function updatePerplexityRecency(recency: string) {
		const config = model.independent_search;
		if (config?.provider === 'perplexity_search') {
			model.independent_search = {
				...config,
				recency_filter: recency ? (recency as NonNullable<typeof config.recency_filter>) : null,
			};
		}
	}

	function updateMistralPremium(premium: boolean) {
		const config = model.independent_search;
		if (config?.provider === 'mistral_web_search') {
			model.independent_search = { ...config, premium };
		}
	}
</script>

{#if ['deepseek', 'perplexity', 'mistral', 'zhipu'].includes(providerId)}
	<section class="independent-search" aria-label="独立搜索 API 设置">
		<div class="search-heading">
			<div>
				<h4>独立搜索 API</h4>
				<p>启用后，模型可调用对应 Provider 的搜索服务；此项按模型配置保存。</p>
			</div>
			<label class="search-toggle" for={fieldId}>
				<input
					id={fieldId}
					type="checkbox"
					checked={model.independent_search != null}
					onchange={(event) => setEnabled(event.currentTarget.checked)}
				/>
				<span>启用</span>
			</label>
		</div>

		{#if model.independent_search?.provider === 'deepseek_search'}
			<div class="search-options">
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-count">返回结果数</label>
					<MaterialNumberField
						id="{fieldId}-count"
						value={model.independent_search.count}
						min={1}
						max={50}
						onChange={updateDeepSeekCount}
					/>
				</div>
				<label class="option-toggle">
					<input
						type="checkbox"
						checked={model.independent_search.need_content}
						onchange={(event) => updateDeepSeekContent(event.currentTarget.checked)}
					/>
					<span>请求网页正文内容</span>
				</label>
			</div>
		{:else if model.independent_search?.provider === 'perplexity_search'}
			<div class="search-options">
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-type">搜索类型</label>
					<MaterialSelect
						id="{fieldId}-type"
						value={model.independent_search.search_type}
						ariaLabel="Perplexity 搜索类型"
						options={[
							{ value: 'web', label: 'Web' },
							{ value: 'fast', label: 'Fast' },
							{ value: 'people', label: 'People' },
						]}
						onChange={withStringValue(updatePerplexityType)}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-results">最多结果数</label>
					<MaterialNumberField
						id="{fieldId}-results"
						value={model.independent_search.max_results}
						min={1}
						max={model.independent_search.search_type === 'people' ? 50 : 20}
						onChange={updatePerplexityResults}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-recency">时间范围</label>
					<MaterialSelect
						id="{fieldId}-recency"
						value={model.independent_search.recency_filter ?? ''}
						ariaLabel="Perplexity 搜索时间范围"
						options={[
							{ value: '', label: '不限时间' },
							{ value: 'hour', label: '最近一小时' },
							{ value: 'day', label: '最近一天' },
							{ value: 'week', label: '最近一周' },
							{ value: 'month', label: '最近一月' },
							{ value: 'year', label: '最近一年' },
						]}
						onChange={withStringValue(updatePerplexityRecency)}
					/>
				</div>
			</div>
		{:else if model.independent_search?.provider === 'mistral_web_search'}
			<div class="search-options">
				<label class="option-toggle">
					<input
						type="checkbox"
						checked={model.independent_search.premium}
						onchange={(event) => updateMistralPremium(event.currentTarget.checked)}
					/>
					<span>使用 Premium Web Search</span>
				</label>
				<p class="search-hint">Premium 搜索会增加已验证新闻来源。</p>
			</div>
		{:else if model.independent_search?.provider === 'zhipu_web_search'}
			<p class="search-hint">将调用智谱 Web-Search-Pro；当前 API 固定使用产品默认参数。</p>
		{/if}
	</section>
{/if}

<style>
	.independent-search {
		display: grid;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
	}
	.search-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.search-heading h4 {
		margin: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-medium-size);
	}
	.search-heading p,
	.search-hint {
		margin: var(--md-sys-space-2xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.search-options {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
		gap: var(--md-sys-space-sm) var(--md-sys-space-md);
		align-items: end;
	}
	.search-toggle,
	.option-toggle {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-height: var(--md-comp-button-touch-height);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		cursor: pointer;
	}
	.search-toggle input,
	.option-toggle input {
		accent-color: var(--md-sys-color-primary);
		width: var(--md-comp-checkbox-size);
		height: var(--md-comp-checkbox-size);
		margin: 0;
	}
	.field-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
	}
	.settings-field-layout {
		display: grid;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	@media (max-width: 455px) {
		.search-heading {
			flex-direction: column;
		}
	}
</style>
