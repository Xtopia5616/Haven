<script>
	let {
		activeSessionId = null,
		showSessionMenu = false,
		sessionMenuOpen = false,
		parallelSessions = [],
		menuSessions = [],
		sessionStatusLabel = /** @type {(session: any) => string} */ ((session) => session.status),
		onToggleSessionMenu = () => {},
		onNewSession = () => {},
		onSwitchSession = () => {},
		onEndSession = () => {},
		messagesLength = 0,
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

<div class="session-switch">
	<button
		class="md-btn md-btn--outlined session-switch-btn"
		onclick={() => onToggleSessionMenu()}
		title={showSessionMenu ? '切换并行会话或开始新会话' : '开始一个新会话'}
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
			stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19" /><line
				x1="5"
				y1="12"
				x2="19"
				y2="12"
			/></svg
		>
		{#if showSessionMenu}
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
		{/if}
		{#if parallelSessions.length > 0}
			<span class="session-switch-badge">{parallelSessions.length}</span>
		{/if}
	</button>
	{#if sessionMenuOpen}
		<div class="session-menu">
			<div class="session-menu-title">正在执行的会话</div>
			{#each menuSessions as session}
				<button
					class="session-menu-item"
					class:selected={session.id === activeSessionId}
					onclick={() => onSwitchSession(session.id)}
					type="button"
				>
					<span class="session-menu-item-main">
						<span class="session-menu-item-title">{session.title}</span>
						<span class="session-menu-item-id">{session.id}</span>
					</span>
					<span
						class="session-menu-item-status"
						class:running={session.status === 'running'}
					>
						{sessionStatusLabel(session)}
					</span>
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
{#if activeSessionId && messagesLength > 0}
	<button
		class="md-btn md-btn--outlined end-session-btn"
		onclick={() => onEndSession()}
		aria-label="结束会话"
		title="结束当前会话"
		type="button"
	>
		<svg
			width="18"
			height="18"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"><rect x="6" y="6" width="12" height="12" rx="2" /></svg
		>
	</button>
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
	.session-switch-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
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
		font-size: 11px;
		font-weight: 700;
		line-height: 18px;
		text-align: center;
		font-variant-numeric: tabular-nums;
	}
	.end-session-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		color: var(--md-sys-color-error);
		border-color: var(--md-sys-color-error);
	}
	.end-session-btn:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.session-menu {
		position: absolute;
		left: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-width: 320px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.session-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.session-menu-item {
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
	.session-menu-item:hover {
		background: var(--md-sys-color-surface-container-highest);
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
	.session-menu-item-title,
	.session-menu-item-id {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-menu-item-id {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		font-family: var(--md-sys-typescale-body-small-font-family, inherit);
	}
	.session-menu-item-status {
		flex-shrink: 0;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-menu-item-status.running {
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
		height: 40px;
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-corner-medium, 8px);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container, transparent);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		line-height: 1;
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
		font-size: 10px;
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
</style>
