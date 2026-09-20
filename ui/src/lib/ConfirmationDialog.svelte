<script>
	import { tick } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import Icon from './Icon.svelte';
	import MenuItem from './MenuItem.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialSplitButton from './MaterialSplitButton.svelte';

	const TIMEOUT_SECONDS = 120;
	/** @type {Record<string, string>} */
	const RISK_LABELS = {
		safe: '安全',
		low: '低风险',
		medium: '中风险',
		high: '高风险',
		critical: '关键操作',
	};

	let {
		stepId,
		toolName,
		sessionId,
		sessionTitle,
		riskLevel,
		summary,
		permissionKey,
		deadlineAt,
		onConfirm,
	} = $props();
	let remaining = $state(TIMEOUT_SECONDS);
	let showDenyMenu = $state(false);
	let confirmAlways = $state(false);
	let dialogEl = /** @type {HTMLDivElement | null} */ ($state(null));

	let normalizedRisk = $derived(String(riskLevel || 'medium').toLowerCase());
	let riskLabel = $derived(RISK_LABELS[normalizedRisk] || '中风险');
	let timeoutPercent = $derived(Math.min(100, Math.max(0, (remaining / TIMEOUT_SECONDS) * 100)));

	// Unbounded tools: Always allow covers every future invocation, not just
	// the summary shown above.
	const UNBOUNDED_ALWAYS = new Set(['shell']);
	let alwaysWarn = $derived(
		UNBOUNDED_ALWAYS.has(String(toolName || '').split(':')[0]) ||
			String(toolName || '').startsWith('shell'),
	);

	$effect(() => {
		const sid = stepId;
		if (!sid) return;
		showDenyMenu = false;
		confirmAlways = false;
		let disposed = false;
		const deadline = deadlineAt || Date.now() + TIMEOUT_SECONDS * 1000;
		let id = /** @type {ReturnType<typeof setInterval> | undefined} */ (undefined);
		const tickCountdown = () => {
			remaining = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
			if (remaining <= 0) {
				if (id) clearInterval(id);
				onConfirm?.({ stepId: sid, approved: false, effect: 'deny', scope: 'once' });
			}
		};
		void tick().then(() => {
			if (!disposed) dialogEl?.focus();
		});
		id = setInterval(tickCountdown, 1000);
		tickCountdown();
		return () => {
			disposed = true;
			if (id) clearInterval(id);
		};
	});

	function decide(/** @type {string} */ effect, /** @type {string} */ scope) {
		showDenyMenu = false;
		onConfirm?.({
			stepId,
			approved: effect === 'allow',
			effect,
			scope,
		});
	}

	function handleAlwaysAllow() {
		if (alwaysWarn && !confirmAlways) {
			confirmAlways = true;
			return;
		}
		decide('allow', 'always');
	}

	/** @param {MouseEvent} event */
	function handleOverlayClick(event) {
		// Preserve the fail-closed behavior of the old modal: dismissing the
		// backdrop is an explicit one-shot denial, never an approval.
		if (event.target === event.currentTarget) decide('deny', 'once');
	}

	/** @param {KeyboardEvent} event */
	function handleWindowKeydown(event) {
		if (stepId && event.key === 'Escape') {
			event.preventDefault();
			decide('deny', 'once');
		}
	}
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#if stepId}
	<div
		class="overlay"
		role="presentation"
		onclick={handleOverlayClick}
		in:fade={{ duration: 220, easing: cubicOut }}
	>
		<div
			class="dialog"
			role="dialog"
			aria-modal="true"
			aria-labelledby="permission-dialog-title"
			aria-describedby="permission-dialog-summary"
			tabindex="-1"
			bind:this={dialogEl}
			onclick={(event) => event.stopPropagation()}
			onkeydown={() => {}}
			in:scale={{ start: 0.96, duration: 260, easing: cubicOut }}
		>
			<header class="dialog-header">
				<div class="header-main">
					<div class="security-icon" aria-hidden="true">
						<Icon name="alertTriangle" size={22} />
					</div>
					<div class="header-copy">
						<div class="eyebrow">权限确认</div>
						<h2 id="permission-dialog-title">需要你的许可</h2>
						<p>Haven 会在执行前等待你的决定</p>
					</div>
				</div>
				<div
					class="risk-badge"
					class:risk-safe={normalizedRisk === 'safe' || normalizedRisk === 'low'}
					class:risk-medium={normalizedRisk === 'medium'}
					class:risk-high={normalizedRisk === 'high' || normalizedRisk === 'critical'}
				>
					<span class="risk-dot" aria-hidden="true"></span>
					{riskLabel}
				</div>
			</header>

			<div class="dialog-body">
				<section class="operation-card" aria-label="待执行操作">
					<div class="section-label">待执行操作</div>
					<div class="operation-name">{toolName || '未命名操作'}</div>
					{#if sessionTitle}
						<div class="operation-context">
							<Icon name="chat" size={15} />
							<span>{sessionTitle}</span>
						</div>
					{:else if sessionId}
						<div class="operation-context">
							<Icon name="chat" size={15} />
							<span>{sessionId}</span>
						</div>
					{/if}
					{#if permissionKey}
						<div class="permission-key">
							<Icon name="key" size={14} />
							<span>授权范围</span>
							<code>{permissionKey}</code>
						</div>
					{/if}
				</section>

				<section class="summary" id="permission-dialog-summary">
					<div class="section-label">操作说明</div>
					<p>{summary || '此操作需要你的许可。'}</p>
				</section>

				<div
					class="timeout"
					class:warn={remaining <= 15}
					class:danger={remaining <= 5}
					role="status"
					aria-live="polite"
				>
					<div class="timeout-head">
						<span class="timeout-label"
							><Icon name="clock" size={15} />自动拒绝倒计时</span
						>
						<strong>{remaining}s</strong>
					</div>
					<div class="timeout-track" aria-hidden="true">
						<span class="timeout-bar" style={`width: ${timeoutPercent}%`}></span>
					</div>
				</div>
			</div>

			<footer class="dialog-footer">
				<div class="action-heading">
					<span>选择允许范围</span>
					<span class="action-hint">范围越大，后续询问越少</span>
				</div>
				{#if confirmAlways}
					<div class="always-warning" role="alert">
						<Icon name="alertTriangle" size={16} />
						<span
							>这会永久允许「{toolName}」的全部后续调用，不限本次参数。请再次点击确认。</span
						>
					</div>
				{/if}
				<div class="actions">
					<MaterialSplitButton
						label="拒绝"
						variant="tonal"
						className="deny-split"
						open={showDenyMenu}
						onclick={() => decide('deny', 'once')}
						onToggle={() => (showDenyMenu = !showDenyMenu)}
						ariaLabel="更多拒绝选项"
					>
						{#snippet children()}
							{#if showDenyMenu}
								<div class="deny-menu" role="menu">
									<MenuItem
										label="本对话拒绝此工具"
										onSelect={() => decide('deny', 'session')}
									/>
									<MenuItem
										label="始终拒绝"
										danger
										onSelect={() => decide('deny', 'always')}
									/>
								</div>
							{/if}
						{/snippet}
					</MaterialSplitButton>
					<div class="allow-group">
						<MaterialButton
							variant="text"
							className="btn-once"
							label="本次允许"
							onclick={() => decide('allow', 'once')}
						/>
						<MaterialButton
							variant="tonal"
							className="btn-session"
							label="本对话允许"
							onclick={() => decide('allow', 'session')}
						/>
						<MaterialButton
							variant={confirmAlways ? 'danger' : 'filled'}
							className="btn-always"
							label={confirmAlways ? '确认始终允许' : '始终允许'}
							title={alwaysWarn ? '将永久允许该工具的全部调用，不限本次参数' : ''}
							onclick={handleAlwaysAllow}
						/>
					</div>
				</div>
			</footer>
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: var(--md-sys-space-xl);
		background: color-mix(in srgb, var(--md-sys-color-scrim) 62%, transparent);
		backdrop-filter: blur(8px);
		z-index: var(--md-sys-z-dialog);
		isolation: isolate;
	}

	.dialog {
		width: min(560px, 100%);
		max-height: min(720px, calc(100vh - 2 * var(--md-sys-space-xl)));
		overflow: auto;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-large);
		box-shadow: var(--md-sys-elevation-5);
		outline: none;
	}

	.dialog-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-2xl) var(--md-sys-space-2xl) var(--md-sys-space-xl);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--md-sys-color-error-container) 72%, transparent),
			transparent 68%
		);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}

	.header-main {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}

	.security-icon {
		display: grid;
		place-items: center;
		width: 44px;
		height: 44px;
		flex: 0 0 auto;
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-error);
	}

	.header-copy {
		min-width: 0;
	}

	.eyebrow,
	.section-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		text-transform: uppercase;
	}

	h2 {
		margin: 2px 0 var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-headline-medium-size);
		font-weight: 750;
		line-height: var(--md-sys-typescale-headline-medium-line-height);
	}

	.header-copy p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}

	.risk-badge {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex: 0 0 auto;
		padding: 6px 10px;
		border-radius: var(--md-sys-shape-full);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		white-space: nowrap;
	}

	.risk-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
	}

	.risk-safe {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}

	.risk-medium {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}

	.risk-high {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}

	.dialog-body {
		display: grid;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-xl) var(--md-sys-space-2xl);
	}

	.operation-card,
	.summary {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
	}

	.operation-card {
		padding: var(--md-sys-space-lg);
		background: var(--md-sys-color-surface-container-low);
	}

	.operation-name {
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		overflow-wrap: anywhere;
	}

	.operation-context,
	.permission-key {
		display: flex;
		align-items: center;
		min-width: 0;
		gap: var(--md-sys-space-xs);
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}

	.operation-context span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.permission-key {
		flex-wrap: wrap;
		padding-top: var(--md-sys-space-sm);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}

	.permission-key code {
		max-width: 100%;
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		overflow-wrap: anywhere;
	}

	.summary {
		padding: var(--md-sys-space-lg);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}

	.summary p {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		white-space: pre-wrap;
	}

	.timeout {
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
	}

	.timeout-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-label-medium-size);
	}

	.timeout-label {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}

	.timeout strong {
		color: var(--md-sys-color-primary);
		font-variant-numeric: tabular-nums;
	}

	.timeout-track {
		height: 5px;
		margin-top: var(--md-sys-space-sm);
		overflow: hidden;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-variant);
	}

	.timeout-bar {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--md-sys-color-primary);
		transition:
			width 1s linear,
			background-color var(--md-sys-motion-duration-short) ease;
	}

	.timeout.warn strong,
	.timeout.warn .timeout-label {
		color: var(--md-sys-color-warning);
	}

	.timeout.warn .timeout-bar {
		background: var(--md-sys-color-warning);
	}

	.timeout.danger strong,
	.timeout.danger .timeout-label {
		color: var(--md-sys-color-error);
	}

	.timeout.danger .timeout-bar {
		background: var(--md-sys-color-error);
	}

	.dialog-footer {
		padding: var(--md-sys-space-lg) var(--md-sys-space-2xl) var(--md-sys-space-2xl);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}

	.action-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-large-size);
		font-weight: 700;
	}

	.action-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 400;
	}

	.always-warning {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}

	.always-warning :global(.icon) {
		flex: 0 0 auto;
		margin-top: 1px;
	}

	.actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
	}

	.allow-group {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: var(--md-sys-space-xs);
		flex-wrap: wrap;
	}

	:global(.md-btn.btn-once) {
		color: var(--md-sys-color-on-surface-variant);
	}

	:global(.md-split-button.deny-split) {
		position: relative;
		flex: 0 0 auto;
	}

	:global(.md-split-button.deny-split .md-btn) {
		--_btn-bg: var(--md-sys-color-error-container);
		--_btn-fg: var(--md-sys-color-on-error-container);
		--_btn-state: var(--md-sys-color-on-error-container);
	}

	.deny-menu {
		position: absolute;
		bottom: calc(100% + var(--md-sys-space-sm));
		left: 0;
		z-index: 2;
		min-width: 190px;
		padding: var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-highest);
		box-shadow: var(--md-sys-elevation-3);
	}

	@media (max-width: 620px) {
		.overlay {
			align-items: flex-end;
			padding: var(--md-sys-space-sm);
		}

		.dialog {
			max-height: calc(100vh - var(--md-sys-space-md));
			border-radius: var(--md-sys-shape-extra-large) var(--md-sys-shape-extra-large)
				var(--md-sys-shape-large) var(--md-sys-shape-large);
		}

		.dialog-header,
		.dialog-body,
		.dialog-footer {
			padding-inline: var(--md-sys-space-lg);
		}

		.actions {
			align-items: stretch;
			flex-direction: column-reverse;
		}

		.allow-group {
			justify-content: stretch;
		}

		.allow-group :global(.md-btn) {
			flex: 1 1 auto;
			min-width: 0;
			padding-inline: var(--md-sys-space-sm);
		}

		:global(.md-split-button.deny-split) {
			align-self: flex-start;
		}
	}

	@media (max-width: 420px) {
		.dialog-header {
			gap: var(--md-sys-space-sm);
			padding-top: var(--md-sys-space-xl);
		}

		.header-main {
			gap: var(--md-sys-space-sm);
		}

		.security-icon {
			width: 36px;
			height: 36px;
		}

		h2 {
			font-size: var(--md-sys-typescale-title-large-size);
			line-height: var(--md-sys-typescale-title-large-line-height);
		}

		.risk-badge {
			padding-inline: 8px;
		}

		.action-heading {
			align-items: flex-start;
			flex-direction: column;
			gap: 2px;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.timeout-bar {
			transition: none;
		}
	}
</style>
