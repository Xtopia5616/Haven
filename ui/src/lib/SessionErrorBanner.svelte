<script>
	import Icon from './Icon.svelte';
	import StatusBadge from './StatusBadge.svelte';

	/**
	 * SessionErrorBanner — keeps a failed conversation visible and explains
	 * why its last run stopped.
	 * @prop {string} reason — sanitized user-visible failure reason
	 */
	let { reason = '' } = $props();
	let displayReason = $derived(reason.trim() || '本次会话因错误停止，暂未收到更具体的原因。');
</script>

<section class="session-error-banner" role="alert" aria-live="assertive">
	<div class="session-error-banner__icon" aria-hidden="true">
		<Icon name="alertTriangle" size={18} />
	</div>
	<div class="session-error-banner__body">
		<div class="session-error-banner__heading">
			<strong>本次会话已停止</strong>
			<StatusBadge label="错误" tone="error" />
		</div>
		<div class="session-error-banner__label">退出原因</div>
		<p class="session-error-banner__reason">{displayReason}</p>
		<p class="session-error-banner__hint">内容已保留，可以使用下方“继续生成”再次尝试。</p>
	</div>
</section>

<style>
	.session-error-banner {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		width: var(--md-sys-chat-agent-max-width);
		max-width: 100%;
		box-sizing: border-box;
		margin: var(--md-sys-space-sm) auto 0;
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-error) 32%, var(--md-sys-color-outline-variant));
		border-left: 3px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-large);
		background: color-mix(
			in srgb,
			var(--md-sys-color-error-container) 34%,
			var(--md-sys-color-surface-container-low)
		);
		color: var(--md-sys-color-on-surface);
	}
	.session-error-banner__icon {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		flex: none;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.session-error-banner__body {
		min-width: 0;
		flex: 1;
	}
	.session-error-banner__heading {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.session-error-banner__label {
		margin-bottom: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-error-banner__reason,
	.session-error-banner__hint {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.session-error-banner__reason {
		color: var(--md-sys-color-on-error-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		white-space: pre-wrap;
	}
	.session-error-banner__hint {
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 640px) {
		.session-error-banner {
			padding-inline: var(--md-sys-space-md);
		}
	}
</style>
