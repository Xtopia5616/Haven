<script lang="ts">
	import Icon from './Icon.svelte';
	import StatusBadge from './StatusBadge.svelte';
	import type { SessionRunEndStatus } from './sessionReducer.ts';

	interface Props {
		status?: SessionRunEndStatus;
		reason?: string;
	}

	/**
	 * SessionRunEndBanner — explains why the current session run ended.
	 */
	let { status = 'error', reason = '' }: Props = $props();
	let isError = $derived(status === 'error');
	let isPaused = $derived(status === 'paused');
	let displayReason = $derived(
		reason.trim() ||
			(isError
				? '本轮因错误停止，暂未收到更具体的原因。'
				: isPaused
					? '本轮已暂停，暂未收到更具体的原因。'
					: '会话已结束。'),
	);
	let heading = $derived(
		isError ? '本轮因错误停止' : isPaused ? '本轮已暂停' : '会话已结束',
	);
	let badge = $derived(isError ? '错误' : isPaused ? '已暂停' : '已结束');
	let hint = $derived(
		isError
			? '已提交的内容仍可恢复；失败的这一步可以使用下方“继续生成”重试。'
			: isPaused
				? '已保留当前输出，可以继续生成或发送新的消息。'
				: '会话内容已保留，可以新建会话继续工作。',
	);
</script>

<section
	class="session-run-end-banner"
	data-status={status}
	role={isError ? 'alert' : 'status'}
	aria-live={isError ? 'assertive' : 'polite'}
>
	<div class="session-run-end-banner__icon" aria-hidden="true">
		<Icon name={isError ? 'alertTriangle' : isPaused ? 'pause' : 'checkCircle'} size={18} />
	</div>
	<div class="session-run-end-banner__body">
		<div class="session-run-end-banner__heading">
			<strong>{heading}</strong>
			<StatusBadge
				label={badge}
				tone={isError ? 'error' : isPaused ? 'warning' : 'success'}
			/>
		</div>
		<div class="session-run-end-banner__label">结束原因</div>
		<p class="session-run-end-banner__reason">{displayReason}</p>
		<p class="session-run-end-banner__hint">{hint}</p>
	</div>
</section>

<style>
	.session-run-end-banner {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		width: var(--md-sys-chat-surface-width);
		max-width: var(--md-sys-chat-surface-width);
		box-sizing: border-box;
		margin: var(--md-sys-space-sm) auto 0;
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-success) 24%, var(--md-sys-color-outline-variant));
		border-left: 3px solid var(--md-sys-color-success);
		border-radius: var(--md-sys-shape-large);
		background: color-mix(
			in srgb,
			var(--md-sys-color-success-container) 34%,
			var(--md-sys-color-surface-container-low)
		);
		color: var(--md-sys-color-on-surface);
		box-shadow: var(--md-sys-elevation-1);
	}
	.session-run-end-banner[data-status='error'] {
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
	.session-run-end-banner[data-status='paused'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-warning) 32%,
			var(--md-sys-color-outline-variant)
		);
		border-left-color: var(--md-sys-color-warning);
		background: color-mix(
			in srgb,
			var(--md-sys-color-warning-container) 34%,
			var(--md-sys-color-surface-container-low)
		);
	}
	.session-run-end-banner__icon {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		flex: none;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.session-run-end-banner[data-status='error'] .session-run-end-banner__icon {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.session-run-end-banner[data-status='paused'] .session-run-end-banner__icon {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.session-run-end-banner__body {
		min-width: 0;
		flex: 1;
	}
	.session-run-end-banner__heading {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.session-run-end-banner__label {
		margin-bottom: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-run-end-banner__reason,
	.session-run-end-banner__hint {
		margin: 0;
		overflow-wrap: anywhere;
	}
	.session-run-end-banner__reason {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		white-space: pre-wrap;
	}
	.session-run-end-banner[data-status='error'] .session-run-end-banner__reason {
		color: var(--md-sys-color-on-error-container);
	}
	.session-run-end-banner__hint {
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 640px) {
		.session-run-end-banner {
			padding-inline: var(--md-sys-space-md);
		}
	}
</style>
