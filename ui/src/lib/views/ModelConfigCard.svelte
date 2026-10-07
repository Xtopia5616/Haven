<script lang="ts">
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialNumberFieldWithUnit from '$lib/MaterialNumberFieldWithUnit.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import { capabilityOptions } from '$lib/modelRoles.ts';
	import { withNumberValue, withStringValue } from '$lib/typedCallbacks.ts';
	import type { CapabilityInput } from '$lib/contracts/generatedCommands.ts';
	import type { ModelDraft, ModelOverrideField } from '$lib/settingsModelTypes.ts';

	interface Props {
		model: ModelDraft;
		providers?: Array<{ name: string }>;
		options?: Array<{ value: string; label: string }>;
		loading?: boolean;
		hasDiscoveredModels?: boolean;
		onRefreshProvider: (provider: string) => void;
		onRenameModel: (model: ModelDraft, nextId: string) => void;
		onSetModel: (model: ModelDraft, modelId: string) => void;
		onSetModelProvider: (model: ModelDraft, provider: string) => void;
		onSetCapability: (model: ModelDraft, capability: CapabilityInput, checked: boolean) => void;
		onUpdateOverride: (model: ModelDraft, field: ModelOverrideField, value: number | null) => void;
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
	const assigned = $derived(!!model.providerName && !!model.model && !needsProviderBinding);
	const configuredCapabilities = $derived(
		capabilityOptions.filter((item) => (model.capabilities || []).includes(item.value)),
	);
	let editorOpen = $state(false);
</script>

<article class="model-card" class:needs-binding={needsProviderBinding}>
	<div class="model-card-heading">
		<MaterialCollapsible lazy bind:open={editorOpen}>
			{#snippet header()}
				<span class="model-summary">
					<span class="model-summary-main">
						<strong class="model-config-id">{model.id}</strong>
						<span class="model-provider-name">
							{#if needsProviderBinding}
								{model.providerName
									? `Provider 不存在：${model.providerName}`
									: '尚未绑定 Provider'}
							{:else}
								{model.providerName} · {model.model || '尚未选择服务模型'}
							{/if}
						</span>
					</span>
					<span class="model-summary-meta">
						<StatusBadge
							label={assigned ? '可用于路由' : '配置未完成'}
							tone={assigned ? 'success' : 'warning'}
						/>
						{#if configuredCapabilities.length}
							<span class="summary-capabilities">
								{configuredCapabilities.map((item) => item.label).join(' · ')}
							</span>
						{:else}
							<span class="summary-capabilities">未声明能力</span>
						{/if}
					</span>
				</span>
			{/snippet}
			{#snippet children()}
				<div class="model-editor">
					<div class="model-fields">
						<div class="model-field settings-field-layout model-id-field">
							<label class="field-label" for="{fieldId}-id">模型配置 ID</label>
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
							<label class="field-label" for="{fieldId}-name">服务模型 ID</label>
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
									label={`刷新 ${model.providerName} 的模型目录`}
									title="刷新模型目录"
									size="dense"
									ariaBusy={loading}
									disabled={!model.providerName || loading}
									onclick={() => onRefreshProvider(model.providerName)}
								/>
							</div>
						</div>
					</div>

					<fieldset class="capability-fieldset">
						<legend class="field-label">模型能力</legend>
						<div class="capability-list">
							{#each capabilityOptions as capability}
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
							<span class="advanced-hint">温度、上下文窗口与用量成本</span>
						{/snippet}
						{#snippet children()}
							<div class="advanced-fields">
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-temperature"
										>温度（默认 0.7）</label
									>
									<MaterialNumberField
										id="{fieldId}-temperature"
										value={model.temperature ?? 0.7}
										step={0.1}
										min={0}
										max={2}
										onChange={withNumberValue((value) =>
											onUpdateOverride(model, 'temperature', value),
										)}
									/>
								</div>
								<div class="model-field settings-field-layout">
									<label class="field-label" for="{fieldId}-context-window"
										>上下文窗口</label
									>
									<MaterialNumberFieldWithUnit
										id="{fieldId}-context-window"
										value={model.context_window != null &&
										model.context_window > 0
											? Math.round(model.context_window / 1000)
											: 0}
										unit="K"
										step={1}
										min={0}
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
			{/snippet}
		</MaterialCollapsible>
		<MaterialIconButton
			icon="delete"
			variant="danger-outline"
			label={`移除模型 ${model.id}`}
			title="移除模型配置"
			size="dense"
			onclick={() => onRemoveModel(model)}
		/>
	</div>
</article>

<style>
	.model-card {
		min-width: 0;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.model-card.needs-binding {
		border-color: var(--md-sys-color-warning);
		background: var(--md-sys-color-warning-container);
	}
	.model-card-heading {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: start;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.model-card-heading :global(.md-collapsible) {
		min-width: 0;
	}
	.model-card-heading :global(.md-collapsible-header) {
		min-height: var(--md-comp-button-touch-height);
	}
	.model-card-heading :global(.md-collapsible-body) {
		margin-top: var(--md-sys-space-md);
	}
	.model-summary {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		width: 100%;
	}
	.model-summary-main,
	.model-summary-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.model-summary-main {
		flex-wrap: wrap;
	}
	.model-config-id {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.model-provider-name,
	.summary-capabilities {
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.summary-capabilities {
		flex: 1;
	}
	.model-editor {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
		padding: 0 0 var(--md-sys-space-sm);
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
		transition:
			opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
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
		transition:
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
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
	.advanced-heading {
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.advanced-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
	}
	@media (max-width: 455px) {
		.model-card-heading {
			padding: var(--md-sys-space-sm);
		}
		.model-summary-meta {
			align-items: flex-start;
			flex-direction: column;
			gap: var(--md-sys-space-xs);
		}
	}
</style>
