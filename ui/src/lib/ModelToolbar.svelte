<script>
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
	<button
		class="md-icon-button model-switch-btn"
		onclick={() => onToggleMenu()}
		title={`切换默认模型${currentModelName ? `：${currentModelName}` : ''}`}
		aria-label="切换默认模型"
		type="button"
	>
		<svg
			width="20"
			height="20"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			><rect x="5" y="5" width="14" height="14" rx="2" /><rect
				x="9.5"
				y="9.5"
				width="5"
				height="5"
			/></svg
		>
	</button>
	{#if modelMenuOpen}
		<div class="model-menu">
			<div class="model-menu-title">切换默认模型</div>
			{#each modelOptions as model}
				<button
					class="model-item"
					class:selected={model.id === currentModelId}
					onclick={() => onModelSelect(model)}
					type="button"
				>
					<span class="model-item-name">{model.name}</span>
					<span class="model-item-provider">{model.provider}</span>
				</button>
			{/each}
			<div class="model-menu-divider"></div>
			<div class="model-menu-title">思考强度</div>
			<div class="effort-row">
				{#each effortOptions as option}
					<button
						class="effort-item"
						class:selected={currentEffort === option.value}
						onclick={() => onEffortSelect(option.value)}
						type="button">{option.label}</button
					>
				{/each}
			</div>
			<div class="model-menu-divider"></div>
			<div class="model-menu-title">联网搜索</div>
			{#if webSearchSupported}
				<div class="effort-row">
					{#each webSearchOptions as option}
						<button
							class="effort-item"
							class:selected={currentWebSearch === option.value}
							onclick={() => onWebSearchSelect(option.value)}
							type="button">{option.label}</button
						>
					{/each}
				</div>
			{:else}
				<div class="model-menu-hint">当前线协议不支持内置联网搜索</div>
				<div class="effort-row">
					<button
						class="effort-item"
						class:selected={currentWebSearch === 'off'}
						onclick={() => onWebSearchSelect('off')}
						type="button">关闭</button
					>
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
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.model-menu-hint {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
		opacity: 0.85;
	}
	.model-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.model-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.model-item.selected .model-item-name {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.model-item-provider {
		font-size: 11px;
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
	.effort-item {
		flex: 1;
		height: 32px;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-weight: 600;
		font-family: inherit;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.effort-item:hover {
		border-color: var(--md-sys-color-primary);
	}
	.effort-item.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
</style>
