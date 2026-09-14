<script>
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';

	let {
		providers = [],
		modelsByProvider = {},
		modelFetching = {},
		refreshingAll = false,
		isProviderKeyConfigured,
		apiStyleLabel,
		onRefreshAll,
		onRefreshProvider,
		onEditProvider,
		onDeleteProvider,
	} = $props();
</script>

<div class="llm-head">
	<div class="llm-head-actions">
		<RefreshButton
			label="刷新模型列表"
			loading={refreshingAll}
			onclick={onRefreshAll}
			disabled={providers.length === 0}
		/>
		<MaterialButton variant="outlined" label="添加 Provider" onclick={onEditProvider} />
	</div>
</div>

{#if providers.length === 0}
	<div class="providers-empty">
		<p class="model-hint">尚未配置任何 Provider。点击「添加 Provider」开始配置。</p>
	</div>
{:else}
	<div class="providers-list">
		{#each providers as provider, idx (provider.name)}
			<div class="provider-card">
				<div class="provider-main">
					<div class="provider-title">
						<span class="provider-name">{provider.name}</span>
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
						{#if modelsByProvider[provider.name]?.length}
							<span class="provider-models"
								>{modelsByProvider[provider.name].length} 个模型</span
							>
						{/if}
					</div>
				</div>
				<div class="provider-actions">
					<RefreshButton
						compact
						iconOnly
						size="dense"
						loading={refreshingAll || !!modelFetching[provider.name]}
						title="刷新模型列表"
						onclick={() => onRefreshProvider(provider.name)}
					/>
					<MaterialIconButton
						size="dense"
						icon="edit"
						label="编辑"
						title="编辑 Provider"
						onclick={() => onEditProvider(idx)}
					/>
					<MaterialIconButton
						size="dense"
						variant="danger"
						icon="delete"
						label="删除"
						title="删除 Provider"
						onclick={() => onDeleteProvider(idx)}
					/>
				</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head-actions,
	.provider-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
		flex-shrink: 0;
		flex-wrap: nowrap;
	}
	.llm-head-actions {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		width: min(100%, 360px);
		margin-left: auto;
	}
	.llm-head-actions :global(.md-btn) {
		width: 100%;
		min-width: 0;
	}
	.provider-actions {
		justify-self: end;
	}
	.providers-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-md);
	}
	.providers-empty {
		margin-top: var(--md-sys-space-md);
	}
	.provider-card {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.provider-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.provider-title {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
		flex-wrap: nowrap;
	}
	.provider-name {
		min-width: 0;
		flex: 1 1 auto;
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.provider-title :global(.api-key-badge) {
		flex: 0 0 auto;
	}
	.provider-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
		overflow: hidden;
	}
	.provider-endpoint {
		min-width: 0;
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: nowrap;
	}
	.provider-models {
		flex: 0 0 auto;
		padding-left: var(--md-sys-space-xs);
		border-left: 1px solid var(--md-sys-color-outline-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-primary);
		white-space: nowrap;
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 700px) {
		.llm-head {
			align-items: flex-start;
			flex-direction: column;
		}
		.llm-head-actions {
			width: 100%;
		}
	}
	@media (max-width: 455px) {
		.provider-card {
			grid-template-columns: 1fr;
			align-items: start;
		}
		.provider-actions {
			width: 100%;
			justify-content: flex-end;
		}
	}
</style>
