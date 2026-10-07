<script lang="ts">
	import type { ChatModelOption } from '$lib/chatModelOperations.ts';
	import type { WebSearchModeInput } from '$lib/contracts/generatedCommands.ts';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialChoiceChip from './MaterialChoiceChip.svelte';
	import MaterialCollapsible from './MaterialCollapsible.svelte';
	import Icon from './Icon.svelte';
	import MenuItem from './MenuItem.svelte';

	type MenuOption = { value: string; label: string };
	interface Props {
		modelMenuOpen?: boolean;
		currentModelName?: string;
		currentModelId?: string;
		modelOptions?: ChatModelOption[];
		onToggleMenu?: () => void;
		onModelSelect?: (model: ChatModelOption) => void | Promise<void>;
		effortOptions?: MenuOption[];
		currentEffort?: string;
		onEffortSelect?: (value: string) => void | Promise<void>;
		webSearchSupported?: boolean;
		webSearchOptions?: Array<{ value: WebSearchModeInput; label: string }>;
		currentWebSearch?: string;
		onWebSearchSelect?: (value: WebSearchModeInput) => void | Promise<void>;
	}

	let {
		modelMenuOpen = false,
		currentModelName = '',
		currentModelId = '',
		modelOptions = [],
		onToggleMenu = () => {},
		onModelSelect = () => {},
		effortOptions = [],
		currentEffort = '',
		onEffortSelect = () => {},
		webSearchSupported = false,
		webSearchOptions = [],
		currentWebSearch = 'off',
		onWebSearchSelect = () => {},
	}: Props = $props();

	let advancedOptionsOpen = $state(false);

	let modelFilter = $state('');
	let switchingModel = $state(false);
	let filteredModelOptions = $derived.by(() => {
		const query = modelFilter.trim().toLocaleLowerCase();
		if (!query) return modelOptions;
		return modelOptions.filter((model) =>
			`${model.name} ${model.providerName} ${model.model}`.toLocaleLowerCase().includes(query),
		);
	});

	function toggleModelMenu() {
		modelFilter = '';
		onToggleMenu();
	}

	async function selectModel(model: ChatModelOption) {
		if (switchingModel) return;
		switchingModel = true;
		modelFilter = '';
		try {
			await onModelSelect(model);
		} finally {
			switchingModel = false;
		}
	}
</script>

<div class="model-switch">
	<MaterialButton
		variant="text"
		className="model-switch-btn"
		onclick={toggleModelMenu}
		title={`切换模型${currentModelName ? `：${currentModelName}` : ''}`}
		ariaLabel={`切换模型${currentModelName ? `：${currentModelName}` : ''}`}
		ariaExpanded={modelMenuOpen}
		ariaHaspopup="dialog"
	>
		{#snippet children()}
			<Icon name="cpu" size={16} />
			<span class="model-switch-label" title={currentModelName || '未选择模型'}
				>{currentModelName || '未选择模型'}</span
			>
			<Icon name={modelMenuOpen ? 'chevronUp' : 'chevronDown'} size={14} />
		{/snippet}
	</MaterialButton>
	{#if modelMenuOpen}
		<div
			class="model-menu"
			role="dialog"
			aria-label="选择模型"
			aria-busy={switchingModel}
		>
			<div class="model-menu-title">选择模型</div>
			{#if switchingModel}
				<div class="model-menu-loading" role="status">正在切换模型…</div>
			{/if}
			{#if modelOptions.length > 0}
				{#if modelOptions.length > 6}
					<input
						class="model-search"
						type="search"
						aria-label="搜索模型"
						placeholder="搜索模型或提供方"
						disabled={switchingModel}
						bind:value={modelFilter}
					/>
				{/if}
				<div class="model-option-list" role="menu" aria-label="可选模型">
					{#each filteredModelOptions as model}
						<MenuItem
							className="model-item"
							disabled={switchingModel}
							selected={model.id === currentModelId}
							role="menuitemradio"
							ariaChecked={model.id === currentModelId}
							onSelect={() => selectModel(model)}
						>
							{#snippet children()}
								<span class="model-item-main">
									<span class="model-item-name">{model.name}</span>
					<span class="model-item-provider">{model.providerName} / {model.model}</span>
								</span>
								{#if model.id === currentModelId}
									<Icon name="check" size={16} />
								{/if}
							{/snippet}
						</MenuItem>
					{:else}
						<div class="model-menu-empty">没有匹配的模型。请尝试搜索模型名或提供方。</div>
					{/each}
				</div>
			{:else}
				<div class="model-menu-empty">还没有可用于对话的模型，请先在设置中配置。</div>
			{/if}

			<MaterialCollapsible lazy bind:open={advancedOptionsOpen}>
				{#snippet header()}
					<span class="advanced-options-title">思考与联网选项</span>
				{/snippet}
				{#snippet children()}
					<div class="model-option-group">
						<div class="model-menu-title">思考强度</div>
						<div class="effort-row" role="group" aria-label="思考强度">
							{#each effortOptions as option}
								<MaterialChoiceChip
									label={option.label}
									selected={currentEffort === option.value}
									disabled={switchingModel}
									onSelect={() => onEffortSelect(option.value)}
								/>
							{/each}
						</div>
					</div>
					<div class="model-option-group">
						<div class="model-menu-title">联网搜索</div>
						{#if webSearchSupported}
							<div class="effort-row" role="group" aria-label="联网搜索">
								{#each webSearchOptions as option}
									<MaterialChoiceChip
										label={option.label}
										selected={currentWebSearch === option.value}
									disabled={switchingModel}
										onSelect={() => onWebSearchSelect(option.value)}
									/>
								{/each}
							</div>
						{:else}
							<div class="model-menu-hint">当前协议不支持内置联网搜索</div>
							<div class="effort-row" role="group" aria-label="联网搜索">
								<MaterialChoiceChip
									label="关闭"
									selected={currentWebSearch === 'off'}
									disabled={switchingModel}
									onSelect={() => onWebSearchSelect('off')}
								/>
							</div>
						{/if}
					</div>
				{/snippet}
			</MaterialCollapsible>
		</div>
	{/if}
</div>

<style>
	.model-switch {
		position: relative;
		flex-shrink: 0;
	}
	:global(.md-btn.model-switch-btn) {
		display: inline-flex;
		align-items: center;
		justify-content: flex-start;
		gap: var(--md-sys-space-xs);
		width: auto;
		min-width: 0;
		max-width: min(240px, 30vw);
		height: var(--md-comp-toolbar-height);
		padding: 0 var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.md-btn.model-switch-btn:hover) {
		background: var(--md-sys-color-surface-container-highest);
		border-color: var(--md-sys-color-outline);
	}
	.model-switch-label {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.model-menu {
		position: absolute;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: var(--md-comp-settings-control-width);
		max-height: 360px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.model-menu-title {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.model-menu-loading {
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-xs);
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.model-search {
		width: calc(100% - 2 * var(--md-sys-space-md));
		margin: 0 var(--md-sys-space-md) var(--md-sys-space-xs);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface);
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.model-search:focus-visible {
		border-color: var(--md-sys-color-primary);
		outline: 2px solid color-mix(in srgb, var(--md-sys-color-primary) 30%, transparent);
	}
	.model-menu-hint {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
		opacity: 0.85;
	}
	.model-menu-empty {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	:global(.menu-item.model-item) {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.model-item-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.model-item-name,
	.model-item-provider {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	:global(.menu-item.model-item:hover) {
		background: var(--md-sys-color-surface-container-highest);
	}
	:global(.menu-item.model-item.selected) .model-item-name {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.model-item-provider {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.model-menu > :global(.md-collapsible) {
		margin-top: var(--md-sys-space-xs);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm) 0;
	}
	.model-menu > :global(.md-collapsible) :global(.md-collapsible-header) {
		min-height: var(--md-comp-button-touch-height);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.advanced-options-title {
		font-weight: 600;
	}
	.model-option-group + .model-option-group {
		margin-top: var(--md-sys-space-xs);
	}
	.effort-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
	}
	.effort-row :global(.md-choice-chip) {
		flex: 1;
		min-width: 0;
		height: 32px;
		padding: 0;
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-family: inherit;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.effort-row :global(.md-choice-chip:hover) {
		border-color: var(--md-sys-color-primary);
	}
	.effort-row :global(.md-choice-chip.selected) {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.effort-row :global(.md-choice-chip:disabled) {
		border-color: var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
	}
	@media (max-width: 640px) {
		:global(.md-btn.model-switch-btn) {
			max-width: min(170px, 34vw);
		}
	}
</style>
