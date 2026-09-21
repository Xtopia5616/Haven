<script>
	import MaterialIconButton from './MaterialIconButton.svelte';

	/**
	 * SessionHeader — keeps the active conversation identity and lifecycle
	 * state visible above the message timeline.
	 * @prop {string} title — active session title
	 * @prop {boolean} hasSession — whether a persisted session is active
	 * @prop {() => void} onNew — create a fresh conversation
	 * @prop {() => void} onEnd — complete the active conversation
	 */
	let { title = '新会话', hasSession = false, onNew, onEnd } = $props();
</script>

<header class="session-header">
	<div class="session-header__identity">
		<div class="session-header__title-row">
			<h1>{title}</h1>
		</div>
	</div>
	<div class="session-header__actions">
		<MaterialIconButton
			size="toolbar"
			variant="default"
			className="session-header__new"
			label="新建会话"
			title="新建会话"
			icon="plus"
			onclick={() => onNew?.()}
		/>
		{#if hasSession}
			<MaterialIconButton
				size="toolbar"
				variant="success-outline"
				className="session-header__end"
				label="完成会话"
				title="完成会话"
				icon="check"
				onclick={() => onEnd?.()}
			/>
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
		align-items: center;
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
