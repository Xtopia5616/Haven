<script>
	import MaterialIconButton from './MaterialIconButton.svelte';

	/**
	 * SessionHeader — keeps the active conversation identity and lifecycle
	 * state visible above the message timeline.
	 * @prop {string} title — active session title
	 * @prop {string} status — user-facing lifecycle status
	 * @prop {boolean} running — whether the session can be stopped
	 * @prop {boolean} hasSession — whether a persisted session is active
	 * @prop {() => void} onNew — create a fresh conversation
	 * @prop {() => void} onEnd — close the active conversation
	 */
	let {
		title = '新会话',
		status = '就绪',
		running = false,
		hasSession = false,
		onNew,
		onEnd,
	} = $props();

	let statusVariant = $derived(
		running
			? 'success'
			: status.includes('暂停') || status.includes('等待')
				? 'warning'
				: 'neutral',
	);
</script>

<header class="session-header">
	<div class="session-header__identity">
		<span class="session-header__eyebrow">
			<span class="session-header__marker" aria-hidden="true"></span>
			当前会话
		</span>
		<div class="session-header__title-row">
			<h1>{title}</h1>
			<span class="md-badge session-header__status" data-variant={statusVariant}
				>{status}</span
			>
		</div>
	</div>
	<div class="session-header__actions">
		<MaterialIconButton
			size="toolbar"
			variant="default"
			className="session-header__new"
			label="新建会话"
			title="新建会话"
			onclick={() => onNew?.()}
		>
			<svg
				class="session-header__icon"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				aria-hidden="true"
			>
				<path d="M12 5v14M5 12h14" />
			</svg>
		</MaterialIconButton>
		{#if hasSession}
			<MaterialIconButton
				size="toolbar"
				variant="danger-outline"
				className="session-header__end"
				label="结束会话"
				title="结束会话"
				onclick={() => onEnd?.()}
			>
				<svg
					class="session-header__icon"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					aria-hidden="true"
				>
					<path d="M6 6l12 12M18 6L6 18" />
				</svg>
			</MaterialIconButton>
		{/if}
	</div>
</header>

<style>
	.session-header {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-md) var(--md-sys-space-2xl);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: color-mix(
			in srgb,
			var(--md-sys-color-surface-container-low) 92%,
			var(--md-sys-color-primary) 8%
		);
		min-width: 0;
	}
	.session-header::before {
		content: '';
		position: absolute;
		inset: 0 auto 0 0;
		width: 3px;
		background: var(--md-sys-color-primary);
		opacity: 0.72;
	}
	.session-header__identity {
		min-width: 0;
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--md-sys-space-xs);
	}
	.session-header__eyebrow {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		letter-spacing: var(--md-sys-typescale-label-letter-spacing);
		white-space: nowrap;
	}
	.session-header__marker {
		width: 6px;
		height: 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--md-sys-color-primary) 14%, transparent);
	}
	.session-header__title-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.session-header h1 {
		min-width: 0;
		max-width: min(52vw, 520px);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 700;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.session-header__status {
		flex-shrink: 0;
	}
	.session-header__actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
		flex-wrap: wrap;
	}
	:global(.session-header__icon) {
		width: var(--md-sys-icon-size);
		height: var(--md-sys-icon-size);
		flex-shrink: 0;
	}
	@media (max-width: 640px) {
		.session-header {
			align-items: center;
			padding: var(--md-sys-space-md);
			padding-left: var(--md-sys-space-lg);
		}
		.session-header__title-row {
			align-items: center;
			flex-direction: row;
			gap: var(--md-sys-space-sm);
		}
		.session-header h1 {
			max-width: min(56vw, 260px);
			font-size: var(--md-sys-typescale-title-medium-size);
		}
	}
</style>
