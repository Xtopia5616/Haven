<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { notificationStore } from './notificationStore.ts';
	import type { Notification } from './notificationStore.ts';
	import { getStatusColorTokens } from './statusColors.ts';
	import Icon from './Icon.svelte';
	// The container stays mounted (empty when idle) and items are only
	// populated after its first render: toasts inserted into a freshly
	// created each block play no intro transition (the block effect has not
	// run a reaction yet), which made the first toast appear instantly.
	let items = $state<Notification[]>([]);
	let mounted = $state(false);
	let unsub: (() => void) | null = null;
	onMount(async () => {
		mounted = true;
		await tick();
		unsub = notificationStore.subscribe((v) => (items = v));
	});
	onDestroy(() => unsub?.());

	function getToastStyle(type: Notification['type'] | undefined) {
		const { dot, background, foreground } = getStatusColorTokens(type || 'info');
		return `--toast-accent: ${dot}; --toast-background: ${background}; --toast-foreground: ${foreground};`;
	}

	function getToastLabel(type: Notification['type']) {
		switch (type) {
			case 'success':
				return '操作成功';
			case 'error':
				return '操作失败';
			case 'warning':
				return '需要留意';
			default:
				return '通知';
		}
	}
</script>

{#if mounted}
	<div class="toast-container" role="region" aria-label="通知">
		{#each items as item (item.id)}
			<div
				class="toast toast-{item.type || 'info'}"
				style={getToastStyle(item.type)}
				role={item.type === 'error' ? 'alert' : 'status'}
				aria-live={item.type === 'error' ? 'assertive' : 'polite'}
				aria-label={`${getToastLabel(item.type)}：${item.msg}`}
				in:fly={{ x: 24, y: 8, duration: 280, easing: cubicOut }}
			>
				<span class="toast-icon">
					<Icon
						name={item.type === 'success'
							? 'checkCircle'
							: item.type === 'error'
								? 'xCircle'
								: item.type === 'warning'
									? 'alertTriangle'
									: 'info'}
						size={20}
					/>
				</span>
				<span class="toast-content">
					<span class="toast-label">{getToastLabel(item.type)}</span>
					<span class="toast-msg">{item.msg}</span>
				</span>
			</div>
		{/each}
	</div>
{/if}

<style>
	.toast-container {
		position: fixed;
		bottom: max(var(--md-sys-space-xl), env(safe-area-inset-bottom));
		right: var(--md-sys-content-gutter);
		z-index: var(--md-sys-z-toast);
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: var(--md-sys-space-md);
		max-height: calc(100vh - 2 * var(--md-sys-space-xl));
		overflow-y: auto;
		overscroll-behavior: contain;
		padding: 2px;
	}
	@supports (height: 100dvh) {
		.toast-container {
			max-height: calc(100dvh - 2 * var(--md-sys-space-xl));
		}
	}
	.toast {
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--toast-accent) 24%, var(--md-sys-color-outline-variant));
		border-radius: var(--md-sys-shape-large);
		width: min(360px, calc(100vw - 2 * var(--md-sys-content-gutter)));
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		pointer-events: auto;
		background: color-mix(
			in srgb,
			var(--md-sys-color-surface-container-lowest) 94%,
			var(--toast-accent)
		);
		color: var(--md-sys-color-on-surface);
		box-shadow:
			0 14px 34px color-mix(in srgb, var(--md-sys-color-shadow) 18%, transparent),
			0 2px 8px color-mix(in srgb, var(--md-sys-color-shadow) 10%, transparent),
			inset 0 1px 0 color-mix(in srgb, var(--md-sys-color-on-surface) 5%, transparent);
		backdrop-filter: blur(18px) saturate(1.18);
		-webkit-backdrop-filter: blur(18px) saturate(1.18);
	}
	.toast-icon {
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 36px;
		width: 36px;
		height: 36px;
		margin-top: 1px;
		border-radius: var(--md-sys-shape-medium);
		background: var(--toast-background);
		color: var(--toast-foreground);
	}
	.toast-icon :global(svg) {
		width: 20px;
		height: 20px;
	}
	.toast-content {
		display: flex;
		flex: 1;
		min-width: 0;
		flex-direction: column;
		gap: 2px;
	}
	.toast-label {
		color: var(--toast-accent);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-label-letter-spacing);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.toast-msg {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 500;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		overflow-wrap: anywhere;
		white-space: normal;
	}
	.toast-error {
		border-color: color-mix(
			in srgb,
			var(--toast-accent) 36%,
			var(--md-sys-color-outline-variant)
		);
		box-shadow:
			0 14px 34px color-mix(in srgb, var(--md-sys-color-error) 12%, transparent),
			0 2px 8px color-mix(in srgb, var(--md-sys-color-shadow) 10%, transparent),
			inset 0 1px 0 color-mix(in srgb, var(--md-sys-color-on-surface) 5%, transparent);
	}
	@media (max-width: 520px) {
		.toast-container {
			right: max(12px, env(safe-area-inset-right));
			bottom: max(12px, env(safe-area-inset-bottom));
			left: max(12px, env(safe-area-inset-left));
			align-items: stretch;
		}
		.toast {
			width: 100%;
		}
	}
</style>
