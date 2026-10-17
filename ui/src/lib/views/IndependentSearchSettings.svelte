<script lang="ts">
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { withStringValue } from '$lib/typedCallbacks.ts';
	import type { ModelDraft, ProviderDraft } from '$lib/settingsModelTypes.ts';

	interface Props {
		model: ModelDraft;
		provider?: ProviderDraft;
	}

	type SearchConfig = NonNullable<ModelDraft['independent_search']>;
	type DeepSeekConfig = Extract<SearchConfig, { provider: 'deepseek_search' }>;
	type PerplexityConfig = Extract<SearchConfig, { provider: 'perplexity_search' }>;
	type AlibabaConfig = Extract<SearchConfig, { provider: 'aliyun_ai_search' }>;
	const perplexityDateFilters = [
		['last_updated_after_filter', '更新日期晚于'],
		['last_updated_before_filter', '更新日期早于'],
		['search_after_date_filter', '发布于日期之后'],
		['search_before_date_filter', '发布于日期之前'],
	] as const;

	const componentId = $props.id();
	let { model, provider }: Props = $props();
	$effect(() => {
		if (
			model.independent_search?.provider === 'aliyun_ai_search' &&
			model.independentSearchApiKey === undefined
		) {
			model.independentSearchApiKey = '';
		}
	});
	const fieldId = `independent-search-${componentId}`;
	const providerId = $derived(provider?.provider?.trim().toLowerCase() ?? '');

	function setEnabled(enabled: boolean) {
		if (!enabled) {
			model.independent_search = null;
			model.independentSearchApiKey = '';
			return;
		}
		switch (providerId) {
			case 'deepseek':
				model.independent_search = {
					provider: 'deepseek_search',
					count: 10,
					need_content: false,
					pass_original_question: false,
					localization: null,
				};
				break;
			case 'perplexity':
				model.independent_search = {
					provider: 'perplexity_search',
					max_results: 10,
					search_type: 'web',
					recency_filter: null,
					country: null,
					max_tokens: null,
					max_tokens_per_page: null,
					search_language_filter: [],
					search_domain_filter: [],
					last_updated_after_filter: null,
					last_updated_before_filter: null,
					search_after_date_filter: null,
					search_before_date_filter: null,
				};
				break;
			case 'dashscope':
				model.independent_search = {
					provider: 'aliyun_ai_search',
					endpoint: '',
					workspace_name: 'default',
					top_k: 5,
					query_rewrite: true,
					content_type: 'snippet',
					way: 'pro',
					time_range: 'no_limit',
					api_key_ref: null,
				};
				model.independentSearchApiKey = '';
				break;
			case 'mistral':
				model.independent_search = { provider: 'mistral_web_search', premium: false };
				break;
			case 'zhipu':
				model.independent_search = { provider: 'zhipu_web_search' };
				break;
		}
	}

	function updateDeepSeek(patch: Partial<DeepSeekConfig>) {
		const config = model.independent_search;
		if (config?.provider === 'deepseek_search') {
			model.independent_search = { ...config, ...patch };
		}
	}

	function updatePerplexity(patch: Partial<PerplexityConfig>) {
		const config = model.independent_search;
		if (config?.provider === 'perplexity_search') {
			model.independent_search = { ...config, ...patch };
		}
	}

	function updateAlibaba(patch: Partial<AlibabaConfig>) {
		const config = model.independent_search;
		if (config?.provider === 'aliyun_ai_search') {
			model.independent_search = { ...config, ...patch };
		}
	}

	function updatePerplexityType(searchType: string) {
		const config = model.independent_search;
		if (config?.provider === 'perplexity_search') {
			const currentMaxResults = config.max_results ?? 10;
			const maxResults =
				searchType === 'people' ? currentMaxResults : Math.min(currentMaxResults, 20);
			updatePerplexity({
				search_type: searchType as PerplexityConfig['search_type'],
				max_results: maxResults,
			});
		}
	}

	function updatePerplexityList(field: 'search_language_filter' | 'search_domain_filter', value: string) {
		const values = [...new Set(value.split(',').map((item) => item.trim()).filter(Boolean))];
		updatePerplexity({ [field]: values });
	}

	function updatePerplexityNumber(
		field: 'max_tokens' | 'max_tokens_per_page',
		event: Event,
	) {
		const value = (event.currentTarget as HTMLInputElement).value;
		updatePerplexity({ [field]: value ? Number(value) : null });
	}

	function updatePerplexityDate(
		field:
			| 'last_updated_after_filter'
			| 'last_updated_before_filter'
			| 'search_after_date_filter'
			| 'search_before_date_filter',
		event: Event,
	) {
		const value = (event.currentTarget as HTMLInputElement).value.trim();
		updatePerplexity({ [field]: value || null });
	}

	function updateAlibabaContentType(contentType: string) {
		const config = model.independent_search;
		if (config?.provider === 'aliyun_ai_search') {
			updateAlibaba({
				content_type: contentType as AlibabaConfig['content_type'],
				way: config.content_type === 'mainText' && contentType !== 'mainText' && config.way === 'pro-fetch'
					? 'pro'
					: config.way,
			});
		}
	}

	function updateMistralPremium(premium: boolean) {
		const config = model.independent_search;
		if (config?.provider === 'mistral_web_search') {
			model.independent_search = { ...config, premium };
		}
	}
</script>

{#if ['deepseek', 'perplexity', 'dashscope', 'mistral', 'zhipu'].includes(providerId)}
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
						onChange={(count) => updateDeepSeek({ count })}
					/>
				</div>
				<label class="option-toggle">
					<input
						type="checkbox"
						checked={model.independent_search.need_content}
						onchange={(event) => updateDeepSeek({ need_content: event.currentTarget.checked })}
					/>
					<span>请求网页正文内容</span>
				</label>
				<label class="option-toggle">
					<input
						type="checkbox"
						checked={model.independent_search.pass_original_question}
						onchange={(event) => updateDeepSeek({ pass_original_question: event.currentTarget.checked })}
					/>
					<span>允许传递原始问题</span>
				</label>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-localization">搜索定位 IP（可选）</label>
					<input
						id="{fieldId}-localization"
						class="md-input"
						type="text"
						value={model.independent_search.localization ?? ''}
						placeholder="例如 203.0.113.10"
						oninput={(event) => updateDeepSeek({ localization: event.currentTarget.value.trim() || null })}
					/>
				</div>
				<p class="search-hint">IP 仅在这里手动填写时发送；原始问题还需模型在搜索工具调用中提供。</p>
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
						onChange={(max_results) => updatePerplexity({ max_results })}
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
						onChange={(value) => updatePerplexity({ recency_filter: value ? value as NonNullable<PerplexityConfig['recency_filter']> : null })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-country">国家/地区代码</label>
					<input
						id="{fieldId}-country"
						class="md-input"
						maxlength="2"
						placeholder="US"
						value={model.independent_search.country ?? ''}
						oninput={(event) => updatePerplexity({ country: event.currentTarget.value.trim().toUpperCase() || null })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-max-tokens">总网页内容 token 上限</label>
					<input
						id="{fieldId}-max-tokens"
						class="md-input"
						type="number"
						min="1"
						max="1000000"
						value={model.independent_search.max_tokens ?? ''}
						oninput={(event) => updatePerplexityNumber('max_tokens', event)}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-per-page">单页 token 上限</label>
					<input
						id="{fieldId}-per-page"
						class="md-input"
						type="number"
						min="1"
						max="1000000"
						value={model.independent_search.max_tokens_per_page ?? ''}
						oninput={(event) => updatePerplexityNumber('max_tokens_per_page', event)}
					/>
				</div>
				<div class="settings-field-layout wide-field">
					<label class="field-label" for="{fieldId}-languages">语言筛选（ISO 639-1，逗号分隔）</label>
					<input
						id="{fieldId}-languages"
						class="md-input"
						placeholder="en, zh"
						value={(model.independent_search.search_language_filter ?? []).join(', ')}
						oninput={(event) => updatePerplexityList('search_language_filter', event.currentTarget.value)}
					/>
				</div>
				<div class="settings-field-layout wide-field">
					<label class="field-label" for="{fieldId}-domains">域名筛选（最多 20 项，逗号分隔；排除项用 - 前缀）</label>
					<input
						id="{fieldId}-domains"
						class="md-input"
						placeholder="wikipedia.org, -example.com"
						value={(model.independent_search.search_domain_filter ?? []).join(', ')}
						oninput={(event) => updatePerplexityList('search_domain_filter', event.currentTarget.value)}
					/>
				</div>
				{#each perplexityDateFilters as [field, label]}
					<div class="settings-field-layout">
						<label class="field-label" for="{fieldId}-{field}">{label}（MM/DD/YYYY）</label>
						<input
							id="{fieldId}-{field}"
							class="md-input"
							placeholder="01/31/2026"
							value={model.independent_search[field] ?? ''}
							oninput={(event) => updatePerplexityDate(field, event)}
						/>
					</div>
				{/each}
			</div>
		{:else if model.independent_search?.provider === 'aliyun_ai_search'}
			<div class="search-options">
				<div class="settings-field-layout wide-field">
					<label class="field-label" for="{fieldId}-endpoint">AI Search 接入地址</label>
					<input
						id="{fieldId}-endpoint"
						class="md-input"
						value={model.independent_search.endpoint}
						placeholder="https://xxxx-hangzhou.opensearch.aliyuncs.com"
						oninput={(event) => updateAlibaba({ endpoint: event.currentTarget.value.trim() })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-workspace">工作空间</label>
					<input
						id="{fieldId}-workspace"
						class="md-input"
						value={model.independent_search.workspace_name}
						placeholder="default"
						oninput={(event) => updateAlibaba({ workspace_name: event.currentTarget.value.trim() })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-key">AI Search API Key</label>
					<ApiKeyField
						id="{fieldId}-key"
						mode="edit"
						configured={!!model.independent_search.api_key_ref}
						bind:value={model.independentSearchApiKey}
						keepHint="已配置，留空保持不变"
						placeholder="AI Search API Key"
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-top-k">最多结果数</label>
					<MaterialNumberField
						id="{fieldId}-top-k"
						value={model.independent_search.top_k}
						min={1}
						max={50}
						onChange={(top_k) => updateAlibaba({ top_k })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-content-type">内容类型</label>
					<MaterialSelect
						id="{fieldId}-content-type"
						value={model.independent_search.content_type}
						ariaLabel="阿里云搜索内容类型"
						options={[
							{ value: 'snippet', label: 'Snippet' },
							{ value: 'summary', label: 'Summary' },
							{ value: 'mainText', label: '网页正文' },
						]}
						onChange={withStringValue(updateAlibabaContentType)}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-way">搜索策略</label>
					<MaterialSelect
						id="{fieldId}-way"
						value={model.independent_search.way}
						ariaLabel="阿里云搜索策略"
						options={[
							{ value: 'lite', label: 'Lite' },
							{ value: 'pro', label: 'Pro' },
							...(model.independent_search.content_type === 'mainText'
								? [{ value: 'pro-fetch', label: 'Pro Fetch' }]
								: []),
						]}
						onChange={(value) => updateAlibaba({ way: value as AlibabaConfig['way'] })}
					/>
				</div>
				<div class="settings-field-layout">
					<label class="field-label" for="{fieldId}-range">发布时间范围</label>
					<MaterialSelect
						id="{fieldId}-range"
						value={model.independent_search.time_range}
						ariaLabel="阿里云搜索时间范围"
						options={[
							{ value: 'no_limit', label: '不限' },
							{ value: 'day', label: '一天内' },
							{ value: 'week', label: '一周内' },
							{ value: 'month', label: '一个月内' },
							{ value: 'year', label: '一年内' },
						]}
						onChange={(value) => updateAlibaba({ time_range: value as AlibabaConfig['time_range'] })}
					/>
				</div>
				<label class="option-toggle">
					<input
						type="checkbox"
						checked={model.independent_search.query_rewrite}
						onchange={(event) => updateAlibaba({ query_rewrite: event.currentTarget.checked })}
					/>
					<span>启用 Query Rewrite</span>
				</label>
				<p class="search-hint wide-field">API Key 会单独存入 Windows Credential Manager。接入地址和工作空间可在 AI Search 控制台的 API Keys 页面查看。</p>
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
	.wide-field {
		grid-column: 1 / -1;
	}
	@media (max-width: 455px) {
		.search-heading {
			flex-direction: column;
		}
	}
</style>
