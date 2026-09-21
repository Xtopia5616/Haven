<script>
	import Icon from './Icon.svelte';
	import StatusBadge from './StatusBadge.svelte';

	/**
	 * SessionTerminationBanner — shows why a terminal conversation stopped.
	 * @prop {'completed'|'error'} status — terminal session status
	 * @prop {string} reason — sanitized user-visible termination reason
	 */
	let { status = 'error', reason = '' } = $props();
	let isError = $derived(status === 'error');
	let displayReason = $derived(
		reason.trim() || (isError ? '本次会话因错误停止，暂未收到更具体的原因。' : '会话已结束。'),
	);
	let heading = $derived(isError ? '本次会话已停止' : '本次会话已结束');
	let badge = $derived(isError ? '错误' : '已结束');
	let hint = $derived(
		isError
			? '内容已保留，可以使用下方“继续生成”再次尝试。'
			: '会话内容已保留，可以新建会话继续工作。',
	);
</script>

<section
	class="session-termination-banner"
	data-status={status}
	role={isError ? 'alert' : 'status'}
	aria-live={isError ? 'assertive' : 'polite'}
>
	<div class="session-termination-banner__icon" aria-hidden="true">
		<Icon name={isError ? 'alertTriangle' : 'checkCircle'} size={18} />
	</div>
	<div class="session-termination-banner__body">
		<div class="session-termination-banner__heading">
			<strong>{heading}</strong>
			<StatusBadge label={badge} tone={isError ? 'error' : 'success'} />
		</div>
		<div class="session-termination-banner__label">终止原因</div>
		<p class="session-termination-banner__reason">{displayReason}</p>
		<p class="session-termination-banner__hint">{hint}</p>
	</div>
</section>

<style>
	.session-termination-banner {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		width: var(--md-sys-chat-agent-max-width);
		max-width: 100%;
		box-sizing: border-box;
		margin: var(--md-sys-space-sm) auto 0;
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 18%, var(--md-sys-color-outline-variant));
		border-left: 3px solid var(--md-sys-color-primary);
		border-radius: var(--md-sys-shape-large);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 18%,
			var(--md-sys-color-surface-container-low)
		);
		color: var(--md-sys-color-on-surface);
		box-shadow: var(--md-sys-elevation-1);
	}
	.session-termination-banner[data-status='error'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-error) 32%,
			var(--md-sys-color-outline-variant)
		);
		border-left-color: var(--md-sys-color-error);
		background: color-mix(
			in srgb,
			var(--md-sys-color-error-container) 34%,
			var(--md-sys-color-surface-container-low)
		);
	}
	.session-termination-banner__icon {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		flex: none;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.session-termination-banner[data-status='error'] .session-termination-banner__icon {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.session-termination-banner__body {
		min-width: 0;
		flex: 1;
	}
	.session-termination-banner__heading {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.session-termination-banner__label {
		margin-bottom: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-termination-banner__reason,
	.session-termination-banner__hint {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.session-termination-banner__reason {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		white-space: pre-wrap;
	}
	.session-termination-banner[data-status='error'] .session-termination-banner__reason {
		color: var(--md-sys-color-on-error-container);
	}
	.session-termination-banner__hint {
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 640px) {
		.session-termination-banner {
			padding-inline: var(--md-sys-space-md);
		}
	}
</style>
