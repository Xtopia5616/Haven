<script>
	import MaterialIconButton from './MaterialIconButton.svelte';

	let {
		activeSessionId = null,
		showSessionMenu = false,
		sessionMenuOpen = false,
		menuSessions = [],
		sessionStatusLabel = /** @type {(session: any) => string} */ ((session) => session.status),
		onToggleSessionMenu = () => {},
		onNewSession = () => {},
		onSwitchSession = () => {},
		tokenStats = null,
		tokenStatsHint = '暂无统计',
		buildTokenTooltip = () => '',
		formatTokenCount = /** @type {(value: any) => string} */ ((value) => String(value)),
		coalesceTokenTotal = /** @type {(...values: any[]) => number} */ (
			(...values) => values[2] || values[0] + values[1]
		),
		showCumulativeTokens = true,
		contextBudget = null,
	} = $props();
</script>

{#if showSessionMenu}
	<div class="session-switch">
		<MaterialIconButton
			size="toolbar"
			variant="tonal"
			className="session-switch-btn"
			label="切换会话"
			ariaExpanded={sessionMenuOpen}
			onclick={() => onToggleSessionMenu()}
			title="切换并行会话或开始新会话"
		>
			<svg
				class="session-switch-icon"
				width="20"
				height="20"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
				><rect x="4" y="5" width="12" height="12" rx="2" /><path
					d="M8 19h8a4 4 0 0 0 4-4V9"
				/></svg
			>
			<span class="session-switch-label">会话</span>
			<svg
				class="session-switch-caret"
				width="16"
				height="16"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg
			>
			{#if menuSessions.length > 0}
				<span class="session-switch-badge">{menuSessions.length}</span>
			{/if}
		</MaterialIconButton>
		{#if sessionMenuOpen}
			<div class="session-menu" role="menu">
				<div class="session-menu-heading">
					<span class="session-menu-title">切换会话</span>
					<span class="session-menu-count">{menuSessions.length} 个</span>
				</div>
				{#each menuSessions as session}
					<button
						class="session-menu-item"
						class:selected={session.id === activeSessionId}
						onclick={() => onSwitchSession(session.id)}
						role="menuitem"
						type="button"
					>
						<span
							class="session-menu-status-dot"
							class:running={session.status === 'running'}
							class:paused={session.status !== 'running'}
							aria-hidden="true"
						></span>
						<span class="session-menu-item-main">
							<span class="session-menu-item-title"
								>{session.title || '未命名会话'}</span
							>
							<span class="session-menu-item-status"
								>{sessionStatusLabel(session)}</span
							>
						</span>
						{#if session.id === activeSessionId}
							<svg
								class="session-menu-check"
								width="16"
								height="16"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2.5"
								stroke-linecap="round"
								stroke-linejoin="round"
								aria-label="当前会话"><polyline points="20 6 9 17 4 12" /></svg
							>
						{/if}
					</button>
				{/each}
				<div class="session-menu-divider"></div>
				<button
					class="session-menu-item session-menu-new"
					onclick={() => onNewSession()}
					type="button"
				>
					<svg
						width="16"
						height="16"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						><line x1="12" y1="5" x2="12" y2="19" /><line
							x1="5"
							y1="12"
							x2="19"
							y2="12"
						/></svg
					>
					新建会话
				</button>
			</div>
		{/if}
	</div>
{/if}
<div
	class="token-stats"
	class:active={!!tokenStats}
	title={tokenStats ? buildTokenTooltip(tokenStats) : tokenStatsHint}
>
	{#if tokenStats}
		<svg
			class="token-icon"
			width="16"
			height="16"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<path d="M4 6h16M4 12h10M4 18h16" />
		</svg>
		<div class="token-text">
			<span class="token-context"
				>{formatTokenCount(
					showCumulativeTokens
						? coalesceTokenTotal(
								tokenStats.cumulativePromptTokens || 0,
								tokenStats.cumulativeCompletionTokens || 0,
								tokenStats.cumulativeTotalTokens || 0,
								tokenStats.cumulativeCachedTokens || 0,
								tokenStats.cumulativeCacheCreationTokens || 0,
							)
						: tokenStats.contextTokens || tokenStats.promptTokens || 0,
				)}</span
			>
			<span class="token-unit">{showCumulativeTokens ? 'tok' : 'ctx'}</span>
		</div>
		{#if contextBudget && contextBudget.window && !showCumulativeTokens}
			<div
				class="token-budget"
				class:warn={contextBudget.ratio >= 0.75}
				class:danger={contextBudget.ratio >= 0.9}
				aria-label={`上下文使用 ${(contextBudget.ratio * 100).toFixed(0)}%`}
			>
				<div
					class="token-budget-fill"
					style="width: {(contextBudget.ratio * 100).toFixed(1)}%"
				></div>
			</div>
		{/if}
	{:else}
		<svg
			class="token-icon"
			width="16"
			height="16"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<path d="M4 6h16M4 12h10M4 18h16" />
		</svg>
		<span class="token-text token-idle">—</span>
	{/if}
</div>

<style>
	.session-switch {
		position: relative;
		flex-shrink: 0;
	}
	:global(.session-switch-btn) {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		width: auto;
		min-width: 0;
		padding-inline: var(--md-sys-space-sm);
		box-shadow: var(--md-sys-elevation-1);
	}
	.session-switch-icon {
		color: var(--md-sys-color-primary);
	}
	.session-switch-label {
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.session-switch-caret {
		flex-shrink: 0;
	}
	.session-switch-badge {
		min-width: 18px;
		height: 18px;
		padding: 0 5px;
		border-radius: 999px;
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: 18px;
		text-align: center;
		font-variant-numeric: tabular-nums;
	}
	.session-menu {
		position: absolute;
		left: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		width: min(320px, calc(100vw - 2 * var(--md-sys-space-md)));
		min-width: min(240px, calc(100vw - 2 * var(--md-sys-space-md)));
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-sm);
		box-shadow: var(--md-sys-elevation-3);
		animation: session-menu-in var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-emphasized);
	}
	.session-menu-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm) var(--md-sys-space-sm);
	}
	.session-menu-title {
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-menu-count {
		padding: 2px var(--md-sys-space-xs);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-menu-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-height: var(--md-comp-button-touch-height);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
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
	.session-menu-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.session-menu-item.selected {
		background: var(--md-sys-color-primary-container);
	}
	.session-menu-item.selected .session-menu-item-title {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.session-menu-item-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		flex: 1 1 auto;
	}
	.session-menu-item-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-menu-status-dot {
		width: 8px;
		height: 8px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-outline);
		flex-shrink: 0;
	}
	.session-menu-status-dot.running {
		background: var(--md-sys-color-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--md-sys-color-primary) 15%, transparent);
	}
	.session-menu-status-dot.paused {
		background: var(--md-sys-color-tertiary);
	}
	.session-menu-item-status {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-menu-check {
		flex-shrink: 0;
		color: var(--md-sys-color-primary);
	}
	.session-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.session-menu-new {
		justify-content: flex-start;
		gap: var(--md-sys-space-sm);
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.token-stats {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 84px;
		height: var(--md-comp-toolbar-height);
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-comp-button-radius);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container, transparent);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		flex-shrink: 0;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.token-stats.active {
		border-color: var(--md-sys-color-primary);
	}
	.token-icon {
		opacity: 0.75;
		flex-shrink: 0;
	}
	.token-text {
		display: inline-flex;
		gap: 4px;
		align-items: baseline;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.token-context {
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.token-unit {
		opacity: 0.6;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.token-idle {
		opacity: 0.5;
	}
	.token-budget {
		position: relative;
		width: 36px;
		height: 4px;
		border-radius: 999px;
		background: var(--md-sys-color-surface-variant, rgba(0, 0, 0, 0.06));
		overflow: hidden;
		flex-shrink: 0;
	}
	.token-budget-fill {
		position: absolute;
		inset: 0 auto 0 0;
		background: var(--md-sys-color-primary);
		transition:
			width var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard),
			background var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard);
	}
	.token-budget.warn .token-budget-fill {
		background: #c97a00;
	}
	.token-budget.danger .token-budget-fill {
		background: var(--md-sys-color-error, #b3261e);
	}
	@keyframes session-menu-in {
		from {
			opacity: 0;
			transform: translateY(4px) scale(0.98);
		}
		to {
			opacity: 1;
			transform: translateY(0) scale(1);
		}
	}
</style>
