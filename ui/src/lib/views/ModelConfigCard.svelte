<script lang="ts">
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialNumberFieldWithUnit from '$lib/MaterialNumberFieldWithUnit.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import { CONTEXT_WINDOW_BOUNDS } from '$lib/contracts/settings.ts';
	import { CAPABILITY_OPTIONS } from '$lib/modelRoles.ts';
	import { withNumberValue, withStringValue } from '$lib/typedCallbacks.ts';
	import type { CapabilityInput } from '$lib/contracts/generatedCommands.ts';
	import type { SelectOption } from '$lib/selectOption.ts';
	import type { ModelDraft, ModelOverrideField, ProviderDraft } from '$lib/settingsModelTypes.ts';
	import IndependentSearchSettings from './IndependentSearchSettings.svelte';

	interface Props {
		model: ModelDraft;
		providers?: ProviderDraft[];
		options?: SelectOption[];
		loading?: boolean;
		hasDiscoveredModels?: boolean;
		onRefreshProvider: (provider: string) => void;
		onRenameModel: (model: ModelDraft, nextId: string) => void;
		onSetModel: (model: ModelDraft, modelId: string) => void;
		onSetModelProvider: (model: ModelDraft, provider: string) => void;
		onSetCapability: (model: ModelDraft, capability: CapabilityInput, checked: boolean) => void;
		onUpdateOverride: (
			model: ModelDraft,
			field: ModelOverrideField,
			value: number | null,
		) => void;
		onRemoveModel: (model: ModelDraft) => void;
	}

	/**
	 * One provider-bound model entry. The owning settings view supplies every
	 * mutation callback; this component only presents and edits the draft.
	 */
	let {
		model,
		providers = [],
		options = [],
		loading = false,
		hasDiscoveredModels = false,
		onRefreshProvider,
		onRenameModel,
		onSetModel,
		onSetModelProvider,
		onSetCapability,
		onUpdateOverride,
		onRemoveModel,
	}: Props = $props();

	const componentId = $props.id();
	const fieldId = `model-config-${componentId}`;
	let advancedOpen = $state(false);

	const providerOptions = $derived([
		{ value: '', label: '选择 Provider' },
		...providers.map((provider) => ({ value: provider.name, label: provider.name })),
	]);
	const needsProviderBinding = $derived(
		!providers.some((provider) => provider.name === model.providerName),
	);
	const selectedProvider = $derived(
		providers.find((provider) => provider.name === model.providerName),
	);
	const assigned = $derived(!!model.providerName && !!model.model && !needsProviderBinding);
	const configuredCapabilities = $derived(
		CAPABILITY_OPTIONS.filter((item) => (model.capabilities || []).includes(item.value)),
	);
	let editorOpen = $state(false);
</script>

<article
	class="model-card motion-list-item"
	class:needs-binding={needsProviderBinding}
	role="presentation"
>
	<div class="settings-table-row model-summary-row" role="row">
		<div class="settings-table-cell" role="cell" aria-label="模型配置 ID">
			<span class="settings-table-cell-label" aria-hidden="true">模型配置 ID</span>
			<strong class="model-config-id">{model.id}</strong>
		</div>
		<div class="settings-table-cell model-service-summary" role="cell" aria-label="服务模型 ID">
			<span class="settings-table-cell-label" aria-hidden="true">服务模型 ID</span>
			<span class="model-provider-name" title={model.model || '尚未选择服务模型'}>
				{model.model || '尚未选择服务模型'}
			</span>
			{#if needsProviderBinding}
				<span class="binding-hint">
					{model.providerName
						? `Provider 不存在：${model.providerName}`
						: '尚未绑定 Provider'}
				</span>
			{/if}
		</div>
		<div class="settings-table-cell" role="cell" aria-label="模型能力">
			<span class="settings-table-cell-label" aria-hidden="true">模型能力</span>
			<span class="summary-capabilities">
				{configuredCapabilities.length
					? configuredCapabilities.map((item) => item.label).join(' · ')
					: '未声明能力'}
			</span>
		</div>
		<div class="settings-table-cell model-status-cell" role="cell" aria-label="状态">
			<span class="settings-table-cell-label" aria-hidden="true">状态</span>
			<StatusBadge
				label={assigned ? '可用于路由' : '配置未完成'}
				tone={assigned ? 'success' : 'warning'}
			/>
		</div>
		<div class="settings-table-cell settings-table-actions" role="cell" aria-label="操作">
			<span class="settings-table-cell-label" aria-hidden="true">操作</span>
			<MaterialButton
				variant="text"
				width="compact"
				label={editorOpen ? '收起' : '编辑'}
				ariaLabel={`${editorOpen ? '收起' : '编辑'}模型 ${model.id}`}
				ariaExpanded={editorOpen}
				onclick={() => (editorOpen = !editorOpen)}
			/>
			<MaterialButton
				variant="danger"
				width="compact"
				label="移除"
				ariaLabel={`移除模型 ${model.id}`}
				onclick={() => onRemoveModel(model)}
			/>
		</div>
	</div>
	{#if editorOpen}
		<div class="settings-table-row model-editor-panel motion-surface-enter" role="row">
			<div class="settings-table-cell model-editor-cell" role="cell" aria-colspan="5">
				<div class="model-editor" role="region" aria-label={`编辑模型 ${model.id}`}>
					<div class="model-fields">
						<div class="model-field settings-field-layout model-id-field">
							<div class="model-field-copy">
								<label class="field-label" for="{fieldId}-id">模型配置 ID</label>
								<span class="advanced-hint"
									>Haven 内部路由标识，不会发送给 Provider</span
								>
							</div>
							<input
								id="{fieldId}-id"
								class="md-input model-id-input"
								value={model.id}
								aria-label={`模型配置 ID：${model.id}`}
								onchange={(event) =>
									onRenameModel(model, event.currentTarget.value)}
							/>
						</div>
						<div class="model-field settings-field-layout">
							<label class="field-label" for="{fieldId}-provider">Provider</label>
							<MaterialSelect
								id="{fieldId}-provider"
								value={model.providerName || ''}
								options={providerOptions}
								ariaLabel={`为模型 ${model.id} 选择 Provider`}
								onChange={withStringValue((value) =>
									onSetModelProvider(model, value),
								)}
							/>
						</div>
						<div class="model-field settings-field-layout model-service-field">
							<div class="model-field-copy">
								<label class="field-label" for="{fieldId}-name">服务模型 ID</label>
								<span class="advanced-hint"
									>此值会作为 API 请求中的 model 参数发送给 Provider</span
								>
							</div>
							<div class="service-model-control">
								<MaterialAutocomplete
									id="{fieldId}-name"
									value={model.model || ''}
									{options}
									placeholder="从目录选择，也可手动输入"
									{loading}
									onChange={withStringValue((value) => onSetModel(model, value))}
									onFocus={() => {
										if (model.providerName && !hasDiscoveredModels)
											onRefreshProvider(model.providerName);
									}}
								/>
								<MaterialIconButton
									icon="refresh"
									label={model.providerName
										? `刷新 Provider ${model.providerName} 的模型目录`
										: '选择 Provider 后刷新模型目录'}
									title={model.providerName
										? `刷新 Provider ${model.providerName} 的模型目录`
										: '选择 Provider 后刷新模型目录'}
									size="dense"
									ariaBusy={loading}
									disabled={!model.providerName || loading}
									onclick={() => onRefreshProvider(model.providerName)}
								/>
							</div>
						</div>
					</div>
					<IndependentSearchSettings {model} provider={selectedProvider} />

					<fieldset class="capability-fieldset">
						<legend class="field-label">模型能力</legend>
						<div class="capability-list">
							{#each CAPABILITY_OPTIONS as capability}
								<label class="capability-option">
									<input
										id="{fieldId}-{capability.value}"
										type="checkbox"
										class="capability-checkbox-input"
										checked={(model.capabilities || []).includes(
											capability.value,
										)}
										onchange={(event) =>
											onSetCapability(
												model,
												capability.value,
												event.currentTarget.checked,
											)}
									/>
									<span class="capability-checkbox-box" aria-hidden="true"></span>
									<span>{capability.label}</span>
								</label>
							{/each}
						</div>
					</fieldset>

					<MaterialCollapsible lazy bind:open={advancedOpen}>
						{#snippet header()}
							<span class="advanced-heading">高级参数</span>
							<span class="advanced-hint">上下文窗口与用量成本</span>
						{/snippet}
						{#snippet children()}
							<div class="advanced-fields">
								<div class="model-field settings-field-layout">
									<div class="model-field-copy">
										<label class="field-label" for="{fieldId}-context-window"
											>上下文窗口</label
										>
										<span class="advanced-hint">
											0 使用 Provider 上报值或默认窗口；显式值范围为 1K–2,000K
											tokens。
										</span>
									</div>
									<MaterialNumberFieldWithUnit
										id="{fieldId}-context-window"
										value={model.context_window != null &&
										model.context_window > 0
											? Math.round(model.context_window / 1000)
											: 0}
										unit="K"
										step={1}
										min={0}
										max={CONTEXT_WINDOW_BOUNDS.maxTokens / 1000}
										onChange={withNumberValue((value) =>
											onUpdateOverride(
												model,
												'context_window',
												value > 0 ? Math.round(value * 1000) : null,
											),
										)}
									/>
								</div>
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-cost-in"
										>输入成本（美元 / 1K tokens）</label
									>
									<MaterialNumberField
										id="{fieldId}-cost-in"
										value={model.cost_per_1k_input_tokens ?? 0}
										step={0.01}
										min={0}
										onChange={withNumberValue((value) =>
											onUpdateOverride(
												model,
												'cost_per_1k_input_tokens',
												value,
											),
										)}
									/>
								</div>
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-cost-out"
										>输出成本（美元 / 1K tokens）</label
									>
									<MaterialNumberField
										id="{fieldId}-cost-out"
										value={model.cost_per_1k_output_tokens ?? 0}
										step={0.01}
										min={0}
										onChange={withNumberValue((value) =>
											onUpdateOverride(
												model,
												'cost_per_1k_output_tokens',
												value,
											),
										)}
									/>
								</div>
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-cache-read"
										>缓存读取成本（美元 / 1K）</label
									>
									<MaterialNumberField
										id="{fieldId}-cache-read"
										value={model.cost_per_1k_cache_read_tokens ?? 0}
										step={0.01}
										min={0}
										onChange={withNumberValue((value) =>
											onUpdateOverride(
												model,
												'cost_per_1k_cache_read_tokens',
												value,
											),
										)}
									/>
								</div>
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-cache-write"
										>缓存写入成本（美元 / 1K）</label
									>
									<MaterialNumberField
										id="{fieldId}-cache-write"
										value={model.cost_per_1k_cache_write_tokens ?? 0}
										step={0.01}
										min={0}
										onChange={withNumberValue((value) =>
											onUpdateOverride(
												model,
												'cost_per_1k_cache_write_tokens',
												value,
											),
										)}
									/>
								</div>
							</div>
						{/snippet}
					</MaterialCollapsible>
				</div>
			</div>
		</div>
	{/if}
</article>

<style>
	.model-card {
		min-width: 0;
	}
	.model-summary-row {
		min-height: var(--md-comp-button-touch-height);
	}
	.model-card.needs-binding .model-summary-row {
		background: var(--md-sys-color-warning-container);
	}
	.model-service-summary {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-2xs, 2px);
	}
	.model-provider-name,
	.summary-capabilities {
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.model-provider-name {
		color: var(--md-sys-color-on-surface);
	}
	.binding-hint {
		color: var(--md-sys-color-on-warning-container);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.model-status-cell {
		display: flex;
		align-items: center;
	}
	@container settings-content (max-width: 640px) {
		.model-status-cell {
			display: block;
		}
	}
	.model-config-id {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.model-editor-panel {
		background: var(--md-sys-color-surface-container-low);
	}
	.model-editor-cell {
		grid-column: 1 / -1;
		padding: var(--md-sys-space-md);
	}
	.model-editor {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
	}
	.model-fields,
	.advanced-fields {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--md-sys-space-md);
		align-items: stretch;
	}
	.model-field {
		min-width: 0;
	}
	.model-field-copy {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.field-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.model-id-input {
		box-sizing: border-box;
	}
	.service-model-control {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.service-model-control :global(.ma-root) {
		flex: 1;
	}
	.capability-fieldset {
		min-width: 0;
		margin: 0;
		padding: 0;
		border: 0;
	}
	.capability-fieldset legend {
		margin-bottom: var(--md-sys-space-sm);
	}
	.capability-list {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm) var(--md-sys-space-lg);
	}
	.capability-option {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-height: var(--md-comp-button-touch-height);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		cursor: pointer;
		user-select: none;
	}
	.capability-checkbox-input {
		position: absolute;
		inset: 0;
		z-index: 1;
		width: 100%;
		height: 100%;
		margin: 0;
		appearance: none;
		opacity: 0;
		cursor: inherit;
	}
	.capability-checkbox-box {
		position: relative;
		isolation: isolate;
		box-sizing: border-box;
		width: var(--md-comp-checkbox-size);
		height: var(--md-comp-checkbox-size);
		flex: 0 0 var(--md-comp-checkbox-size);
		border: var(--md-comp-checkbox-border-width) solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-extra-small);
		background: transparent;
		color: var(--md-sys-color-on-surface);
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.capability-checkbox-box::before {
		content: '';
		position: absolute;
		z-index: -1;
		top: 50%;
		left: 50%;
		width: var(--md-comp-checkbox-state-layer-size);
		height: var(--md-comp-checkbox-state-layer-size);
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		opacity: 0;
		transform: translate(-50%, -50%);
		transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		pointer-events: none;
	}
	.capability-checkbox-box::after {
		content: '';
		position: absolute;
		top: 50%;
		left: 50%;
		width: 5px;
		height: 9px;
		border: solid var(--md-sys-color-on-primary);
		border-width: 0 2px 2px 0;
		opacity: 0;
		transform: translate(-50%, -60%) rotate(45deg);
		transition: opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
		pointer-events: none;
	}
	.capability-checkbox-input:checked + .capability-checkbox-box {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-primary);
	}
	.capability-checkbox-input:checked + .capability-checkbox-box::after {
		opacity: 1;
	}
	.capability-checkbox-input:focus-visible + .capability-checkbox-box {
		box-shadow: var(--md-sys-focus-ring);
	}
	.capability-option:hover .capability-checkbox-box::before {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.capability-option:active .capability-checkbox-box::before {
		opacity: var(--md-sys-state-pressed-opacity);
	}
	.model-editor > :global(.md-collapsible) {
		padding-top: var(--md-sys-space-sm);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	@container settings-content (min-width: 700px) {
		.model-fields {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	.advanced-heading {
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.advanced-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
