<script>
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
	let { title = '新会话', status = '准备开始', running = false, hasSession = false, onNew, onEnd } = $props();
</script>

<header class="session-header">
	<div class="session-header__identity">
		<span class="session-header__eyebrow">当前会话</span>
		<h1>{title}</h1>
		<span class="md-badge session-header__status" data-variant={running ? 'success' : 'neutral'}>{status}</span>
	</div>
	<div class="session-header__actions">
		<button class="md-btn md-btn--outlined" type="button" onclick={() => onNew?.()}>新建会话</button>
		{#if hasSession}
			<button class="md-btn md-btn--text session-header__end" type="button" onclick={() => onEnd?.()} aria-label="结束会话">结束</button>
		{/if}
	</div>
</header>

<style>
	.session-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		padding: var(--md-sys-space-lg) var(--md-sys-space-2xl) var(--md-sys-space-md);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface);
	}
	.session-header__identity {
		min-width: 0;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.session-header__eyebrow {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-weight: 600;
		white-space: nowrap;
	}
	.session-header h1 {
		min-width: 0;
		max-width: min(52vw, 520px);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 18px;
		font-weight: 700;
		letter-spacing: -0.15px;
	}
	.session-header__status {
		flex-shrink: 0;
	}
	.session-header__actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
	}
	.session-header__end {
		color: var(--md-sys-color-error);
	}
	@media (max-width: 640px) {
		.session-header {
			align-items: flex-start;
			padding-inline: var(--md-sys-space-md);
		}
		.session-header__identity {
			align-items: flex-start;
			flex-direction: column;
			gap: var(--md-sys-space-xs);
		}
		.session-header__actions .md-btn--outlined {
			font-size: 0;
			min-width: var(--md-comp-button-touch-height);
		}
		.session-header__actions .md-btn--outlined::after {
			content: '+';
			font-size: 18px;
		}
	}
</style>
