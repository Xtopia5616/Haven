<script>
	import MaterialIconButton from './MaterialIconButton.svelte';
	import MenuItem from './MenuItem.svelte';

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
	} = $props();
</script>

<div class="model-switch">
	<MaterialIconButton
		size="toolbar"
		className="model-switch-btn"
		onclick={() => onToggleMenu()}
		title={`切换默认模型${currentModelName ? `：${currentModelName}` : ''}`}
		label="切换默认模型"
		icon="cpu"
	>
	</MaterialIconButton>
	{#if modelMenuOpen}
		<div class="model-menu">
			<div class="model-menu-title">切换默认模型</div>
			{#each modelOptions as model}
				<MenuItem
					className="model-item"
					selected={model.id === currentModelId}
					role="menuitemradio"
					ariaChecked={model.id === currentModelId}
					onSelect={() => onModelSelect(model)}
				>
					{#snippet children()}
						<span class="model-item-name">{model.name}</span>
						<span class="model-item-provider">{model.provider}</span>
					{/snippet}
				</MenuItem>
			{/each}
			<div class="model-menu-divider"></div>
			<div class="model-menu-title">思考强度</div>
			<div class="effort-row">
				{#each effortOptions as option}
					<MenuItem
						className="effort-item"
						label={option.label}
						selected={currentEffort === option.value}
						role="menuitemradio"
						ariaChecked={currentEffort === option.value}
						onSelect={() => onEffortSelect(option.value)}
					/>
				{/each}
			</div>
			<div class="model-menu-divider"></div>
			<div class="model-menu-title">联网搜索</div>
			{#if webSearchSupported}
				<div class="effort-row">
					{#each webSearchOptions as option}
						<MenuItem
							className="effort-item"
							label={option.label}
							selected={currentWebSearch === option.value}
							role="menuitemradio"
							ariaChecked={currentWebSearch === option.value}
							onSelect={() => onWebSearchSelect(option.value)}
						/>
					{/each}
				</div>
			{:else}
				<div class="model-menu-hint">当前线协议不支持内置联网搜索</div>
				<div class="effort-row">
					<MenuItem
						className="effort-item"
						label="关闭"
						selected={currentWebSearch === 'off'}
						role="menuitemradio"
						ariaChecked={currentWebSearch === 'off'}
						onSelect={() => onWebSearchSelect('off')}
					/>
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.model-switch {
		position: relative;
		flex-shrink: 0;
	}
	.model-menu {
		position: absolute;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-height: 320px;
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
	.model-menu-hint {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
		opacity: 0.85;
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
	.model-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.effort-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
	}
	:global(.menu-item.effort-item) {
		flex: 1;
		height: 32px;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-family: inherit;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	:global(.menu-item.effort-item:hover) {
		border-color: var(--md-sys-color-primary);
	}
	:global(.menu-item.effort-item.selected) {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
</style>
