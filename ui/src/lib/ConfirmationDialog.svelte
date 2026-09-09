<script>
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import MenuItem from './MenuItem.svelte';
	import MaterialButton from './MaterialButton.svelte';
	import MaterialSplitButton from './MaterialSplitButton.svelte';

	// Interactive dialog countdown (starts when shown). Backend keeps a
	// longer absolute fail-closed ceiling for closed/crashed UI.
	const TIMEOUT_SECONDS = 120;

	// `deadlineAt` (epoch ms) is the backend's deadline for this confirmation:
	// it starts when the request was created, not when the dialog is shown.
	// Queued confirmations therefore count down from their real remaining
	// budget instead of being granted a fresh window the server never honors.
	// Falls back to `now + TIMEOUT_SECONDS` for safety.
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
		const deadline = deadlineAt || Date.now() + TIMEOUT_SECONDS * 1000;
		let id = /** @type {ReturnType<typeof setInterval> | undefined} */ (undefined);
		const tick = () => {
			remaining = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
			if (remaining <= 0) {
				if (id) clearInterval(id);
				onConfirm?.({
					stepId: sid,
					approved: false,
					effect: 'deny',
					scope: 'once',
				});
			}
		};
		id = setInterval(tick, 1000);
		tick();
		return () => {
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

	/**
	 * @param {KeyboardEvent} event
	 */
	function handleOverlayKeydown(event) {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			decide('deny', 'once');
		}
	}
</script>

{#if stepId}
	<div
		class="overlay"
		role="button"
		tabindex="0"
		onclick={() => decide('deny', 'once')}
		onkeydown={handleOverlayKeydown}
		in:fade={{ duration: 300, easing: cubicOut }}
	>
		<div class="dialog" role="presentation" tabindex="-1" onclick={(e) => e.stopPropagation()} in:scale={{ start: 0.92, duration: 450, easing: cubicOut }}>
			<h3>需要确认的操作</h3>
			<div class="detail">
				{#if sessionTitle}<div><strong>会话:</strong> {sessionTitle}</div>{/if}
				<div><strong>工具:</strong> {toolName}</div>
				{#if permissionKey && permissionKey !== toolName}
					<div><strong>权限键:</strong> <code>{permissionKey}</code></div>
				{/if}
				<div>
					<strong>风险:</strong>
					<span class="risk risk-{riskLevel || 'medium'}">{riskLevel || 'medium'}</span>
				</div>
			</div>
			<div class="summary">{summary || '此操作需要你的许可。'}</div>
			<div class="timeout" class:warn={remaining <= 15} class:danger={remaining <= 5}>
				<div class="timeout-bar" style="width: {(remaining / TIMEOUT_SECONDS) * 100}%"></div>
				<span>未确认将于 {remaining} 秒后自动拒绝</span>
			</div>
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
						<div class="deny-menu">
							<MenuItem label="本对话拒绝此工具" onSelect={() => decide('deny', 'session')} />
							<MenuItem label="始终拒绝" danger onSelect={() => decide('deny', 'always')} />
						</div>
					{/if}
					{/snippet}
				</MaterialSplitButton>
				<div class="btn-group">
					<MaterialButton variant="text" className="btn-once" label="仅本次" onclick={() => decide('allow', 'once')} />
					<MaterialButton variant="tonal" className="btn-session" label="本对话允许" onclick={() => decide('allow', 'session')} />
					<MaterialButton
						variant="filled"
						className="btn-always"
						label="始终允许"
						title={alwaysWarn ? '将永久允许该工具的全部调用，不限本次参数' : ''}
						onclick={() => {
							if (
								alwaysWarn &&
								!window.confirm(
									`「始终允许」会永久放行工具「${toolName}」的全部后续调用（不限本次参数）。确认？`,
								)
							) {
								return;
							}
							decide('allow', 'always');
						}}
					/>
				</div>
			</div>
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 55%, transparent);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: var(--md-sys-z-dialog);
	}
	.dialog {
		background: var(--md-sys-color-surface-container-high);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-3xl);
		min-width: min(380px, calc(100vw - 2 * var(--md-sys-content-gutter)));
		max-width: min(560px, 92vw);
		box-shadow: var(--md-sys-elevation-3);
	}
	h3 {
		color: var(--md-sys-color-error);
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-title-large-line-height);
		margin-bottom: var(--md-sys-space-lg);
	}
	.detail {
		margin-bottom: var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.detail strong { color: var(--md-sys-color-on-surface); }
	.detail code {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
	}
	.summary {
		margin: 0 0 var(--md-sys-space-lg);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.risk { font-weight: 700; text-transform: capitalize; }
	.risk-high, .risk-critical { color: var(--md-sys-color-error); }
	.risk-medium { color: var(--md-sys-color-warning); }
	.risk-low { color: var(--md-sys-color-success); }
	.timeout {
		position: relative;
		margin-bottom: var(--md-sys-space-lg);
		border-radius: var(--md-sys-shape-extra-small);
		background: var(--md-sys-color-surface-variant, rgba(0, 0, 0, 0.06));
		overflow: hidden;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: var(--md-sys-space-xs) 0;
		gap: var(--md-sys-space-sm);
	}
	.timeout-bar {
		position: absolute;
		inset: 0 auto 0 0;
		background: var(--md-sys-color-primary);
		opacity: 0.25;
		transition: width 1s linear;
	}
	.timeout span {
		position: relative;
	}
	.timeout.warn .timeout-bar {
		background: var(--md-sys-color-warning, #c97a00);
	}
	.timeout.danger {
		color: var(--md-sys-color-error);
	}
	.timeout.danger .timeout-bar {
		background: var(--md-sys-color-error);
		opacity: 0.45;
	}
	.actions {
		display: flex;
		gap: var(--md-sys-space-sm);
		justify-content: space-between;
		align-items: center;
		flex-wrap: wrap;
	}
	.btn-group {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
		justify-content: flex-end;
	}
	:global(.md-split-button.deny-split) {
		position: relative;
	}
	.deny-menu {
		position: absolute;
		bottom: calc(100% + 4px);
		left: 0;
		min-width: 160px;
		background: var(--md-sys-color-surface-container-highest);
		border-radius: var(--md-sys-shape-small);
		box-shadow: var(--md-sys-elevation-2);
		padding: var(--md-sys-space-xs);
		display: flex;
		flex-direction: column;
		z-index: 1;
	}
</style>
