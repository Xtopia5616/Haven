<script lang="ts">
	import AsyncState from '$lib/AsyncState.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import type { CapabilityInput } from '$lib/contracts/generatedCommands.ts';
	import type { DiscoveredModelMap } from '$lib/contracts/model.ts';
	import type { SelectOption } from '$lib/selectOption.ts';
	import type { ModelDraft, ModelOverrideField, ProviderDraft } from '$lib/settingsModelTypes.ts';
	import ModelConfigCard from './ModelConfigCard.svelte';

	interface Props {
		providers?: ProviderDraft[];
		models?: ModelDraft[];
		modelsByProvider?: DiscoveredModelMap;
		modelFetching?: Record<string, boolean>;
		refreshingAll?: boolean;
		modelOptions: (providerName: string) => SelectOption[];
		isProviderKeyConfigured: (provider: ProviderDraft) => boolean;
		apiStyleLabel: (provider: ProviderDraft) => string;
		onRefreshAll?: () => void;
		onRefreshProvider: (providerName: string) => void;
		onAddModel: (providerName: string) => void;
		onRenameModel: (model: ModelDraft, nextId: string) => void;
		onSetModel: (model: ModelDraft, modelId: string) => void;
		onSetModelProvider: (model: ModelDraft, provider: string) => void;
		onSetCapability: (model: ModelDraft, capability: CapabilityInput, checked: boolean) => void;
		onUpdateOverride: (model: ModelDraft, field: ModelOverrideField, value: number | null) => void;
		onRemoveModel: (model: ModelDraft) => void;
		onEditProvider: (index?: number) => void;
		onDeleteProvider: (index: number) => void;
	}

	let {
		providers = [],
		models = [],
		modelsByProvider = {},
		modelFetching = {},
		refreshingAll = false,
		modelOptions,
		isProviderKeyConfigured,
		apiStyleLabel,
		onRefreshAll,
		onRefreshProvider,
		onAddModel,
		onRenameModel,
		onSetModel,
		onSetModelProvider,
		onSetCapability,
		onUpdateOverride,
		onRemoveModel,
		onEditProvider,
		onDeleteProvider,
	}: Props = $props();

	function modelsForProvider(providerName: string) {
		return models.filter((model) => model.providerName === providerName);
	}
	const unboundModels = $derived.by(() => {
		const providerNames = new Set(providers.map((provider) => provider.name));
		return models.filter((model) => !providerNames.has(model.providerName));
	});
</script>

<div class="provider-toolbar">
	<div class="provider-toolbar-copy">
		<h3>模型服务</h3>
		<p>先连接 Provider，再为它登记模型和能力。</p>
	</div>
	<div class="provider-toolbar-actions">
		<RefreshButton
			label="刷新全部目录"
			loading={refreshingAll}
			onclick={onRefreshAll}
			disabled={providers.length === 0}
		/>
		<MaterialButton variant="filled" label="添加 Provider" onclick={() => onEditProvider()} />
	</div>
</div>

{#if providers.length === 0}
	<AsyncState
		state="unconfigured"
		title="尚未配置 Provider"
		message="添加 API 地址和凭据后，可以刷新服务目录并配置模型。"
		actionLabel="添加第一个 Provider"
		onAction={() => onEditProvider()}
	/>
{/if}

{#if providers.length > 0}
	<div class="providers-list">
		{#each providers as provider, idx (provider.name)}
			{@const providerModels = modelsForProvider(provider.name)}
			{@const catalogLoaded = Array.isArray(modelsByProvider[provider.name])}
			{@const catalogCount = modelsByProvider[provider.name]?.length ?? 0}
			<section class="provider-section" aria-label={`Provider ${provider.name}`}>
				<div class="provider-card">
					<div class="provider-main">
						<div class="provider-title">
							<h4>{provider.name}</h4>
							<ApiKeyField
								mode="badge"
								configured={isProviderKeyConfigured(provider)}
								badgePrefix={apiStyleLabel(provider)}
							/>
						</div>
						<div class="provider-meta">
							<span class="provider-endpoint" title={provider.base_url}
								>{provider.base_url}</span
							>
							<span class="provider-model-count"
								>{providerModels.length} 个模型配置</span
							>
							{#if modelFetching[provider.name] || refreshingAll}
								<StatusBadge label="正在刷新目录" tone="info" />
							{:else if catalogLoaded}
								<StatusBadge
									label={`服务目录 ${catalogCount} 个`}
									tone={catalogCount ? 'success' : 'warning'}
								/>
							{:else}
								<StatusBadge label="目录未加载" tone="neutral" />
							{/if}
						</div>
					</div>
					<div class="provider-actions" aria-label={`${provider.name} 操作`}>
						<RefreshButton
							iconOnly
							size="dense"
							loading={refreshingAll || !!modelFetching[provider.name]}
							title="刷新模型目录"
							onclick={() => onRefreshProvider(provider.name)}
						/>
						<MaterialIconButton
							size="dense"
							icon="edit"
							label={`编辑 Provider ${provider.name}`}
							title="编辑 Provider"
							onclick={() => onEditProvider(idx)}
						/>
						<MaterialIconButton
							size="dense"
							variant="danger-outline"
							icon="delete"
							label={`删除 Provider ${provider.name}`}
							title={providerModels.length
								? '请先移除该 Provider 下的模型'
								: '删除 Provider'}
							onclick={() => onDeleteProvider(idx)}
						/>
					</div>
				</div>

				<div class="provider-models-section">
					<div class="provider-models-heading">
						<div>
							<h5>模型</h5>
							<p>模型配置 ID 会被路由策略引用；服务模型 ID 对应 Provider 目录。</p>
						</div>
						<MaterialButton
							variant="outlined"
							label="添加模型配置"
							onclick={() => onAddModel(provider.name)}
						/>
					</div>
					{#if providerModels.length}
						<div class="provider-model-list">
							{#each providerModels as model (model.id)}
								<ModelConfigCard
									{model}
									{providers}
									options={modelOptions(provider.name)}
									loading={!!modelFetching[provider.name]}
									hasDiscoveredModels={catalogLoaded}
									{onRefreshProvider}
									{onRenameModel}
									{onSetModel}
									{onSetModelProvider}
									{onSetCapability}
									{onUpdateOverride}
									{onRemoveModel}
								/>
							{/each}
						</div>
					{:else}
						<p class="provider-note">
							还没有模型配置。点击「添加模型配置」，再选择目录中的模型 ID。
						</p>
					{/if}
				</div>
			</section>
		{/each}
	</div>
{/if}

{#if unboundModels.length > 0}
	<section class="unbound-section" aria-labelledby="unbound-models-heading">
		<div class="unbound-heading">
			<div>
				<h4 id="unbound-models-heading">需要修复的模型</h4>
				<p>这些模型没有绑定当前已配置的 Provider。请展开并重新选择 Provider 后再保存。</p>
			</div>
			<StatusBadge label={`${unboundModels.length} 个待修复`} tone="warning" />
		</div>
		<div class="provider-model-list">
			{#each unboundModels as model (model.id)}
				<ModelConfigCard
					{model}
					{providers}
					options={modelOptions(model.providerName)}
					loading={!!modelFetching[model.providerName]}
					hasDiscoveredModels={Array.isArray(modelsByProvider[model.providerName])}
					{onRefreshProvider}
					{onRenameModel}
					{onSetModel}
					{onSetModelProvider}
					{onSetCapability}
					{onUpdateOverride}
					{onRemoveModel}
				/>
			{/each}
		</div>
	</section>
{/if}

<style>
	.provider-toolbar {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-lg);
	}
	.provider-toolbar-copy {
		min-width: 0;
	}
	.provider-toolbar-copy h3,
	.provider-toolbar-copy p,
	.provider-models-heading h5,
	.provider-models-heading p,
	.unbound-heading h4,
	.unbound-heading p {
		margin: 0;
	}
	.provider-toolbar-copy h3 {
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.provider-toolbar-copy p,
	.provider-models-heading p,
	.unbound-heading p {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.provider-toolbar-actions {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 1 auto;
		min-width: 0;
		max-width: 100%;
		transform: translateX(calc(0px - var(--md-sys-space-2xl)));
	}
	.provider-toolbar-actions :global(.md-btn:last-child) {
		flex-shrink: 0;
		white-space: nowrap;
	}
	.providers-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
	}
	.provider-section,
	.unbound-section {
		min-width: 0;
		overflow: hidden;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.provider-card {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		background: var(--md-sys-color-surface-container-low);
	}
	.provider-main {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.provider-title {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.provider-title h4 {
		min-width: 0;
		margin: 0;
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: var(--md-sys-typescale-title-large-line-height);
		font-weight: 650;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.provider-title :global(.api-key-badge) {
		flex: 0 0 auto;
	}
	.provider-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
		flex-wrap: wrap;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.provider-endpoint {
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
	}
	.provider-model-count {
		padding-left: var(--md-sys-space-sm);
		border-left: 1px solid var(--md-sys-color-outline-variant);
		white-space: nowrap;
	}
	.provider-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.provider-models-section {
		padding: var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.provider-models-heading,
	.unbound-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-md);
	}
	.provider-models-heading h5 {
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		font-weight: 650;
		color: var(--md-sys-color-on-surface);
	}
	.provider-models-heading p {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.provider-model-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.provider-note {
		margin: 0;
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.unbound-section {
		margin-top: var(--md-sys-space-lg);
		padding: var(--md-sys-space-lg);
		border-color: var(--md-sys-color-warning);
		background: var(--md-sys-color-warning-container);
	}
	.unbound-heading h4 {
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		font-weight: 650;
		color: var(--md-sys-color-on-surface);
	}
	@container settings-content (max-width: 700px) {
		.provider-toolbar {
			align-items: flex-start;
			flex-direction: column;
		}
		.provider-toolbar-actions {
			width: 100%;
			justify-content: flex-end;
		}
		.provider-card {
			grid-template-columns: minmax(0, 1fr);
		}
		.provider-actions {
			justify-content: flex-end;
		}
	}
	@container settings-content (max-width: 455px) {
		.provider-card,
		.provider-models-section,
		.unbound-section {
			padding: var(--md-sys-space-md);
		}
		.provider-title {
			align-items: flex-start;
			flex-direction: column;
		}
		.provider-models-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		.provider-models-heading :global(.md-btn) {
			width: 100%;
		}
		.unbound-heading {
			align-items: flex-start;
			flex-direction: column;
		}
	}
</style>
