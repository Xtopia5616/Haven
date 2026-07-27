<script>
	import { onDestroy } from 'svelte';
	import { notificationStore } from './stores.js';
	let items = $state([]);
	const unsub = notificationStore.subscribe((v) => (items = v));
	onDestroy(() => unsub());

	const icons = {
		success: `<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/></svg>`,
		error: `<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2C6.47 2 2 6.47 2 12s4.47 10 10 10 10-4.47 10-10S17.53 2 12 2zm5 13.59L15.59 17 12 13.41 8.41 17 7 15.59 10.59 12 7 8.41 8.41 7 12 10.59 15.59 7 17 8.41 13.41 12 17 15.59z"/></svg>`,
		warning: `<svg viewBox="0 0 24 24" fill="currentColor"><path d="M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z"/></svg>`,
		info: `<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z"/></svg>`,
	};

	function getIcon(type) {
		return icons[type] || icons.info;
	}
</script>

{#if items.length > 0}
	<div class="toast-container">
		{#each items as item (item.id)}
			<div class="toast toast-{item.type || 'info'}">
				<span class="toast-icon">{@html getIcon(item.type)}</span>
				<span class="toast-msg">{item.msg}</span>
			</div>
		{/each}
	</div>
{/if}

<style>
	.toast-container {
		position: fixed;
		bottom: 80px;
		right: var(--md-sys-space-lg);
		z-index: var(--md-sys-z-toast);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.toast {
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		padding-left: calc(var(--md-sys-space-lg) + 4px);
		border-radius: var(--md-sys-shape-small);
		font-size: 13px;
		font-weight: 600;
		box-shadow: var(--md-sys-elevation-2);
		animation: slideIn var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-emphasized);
		max-width: 360px;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		pointer-events: auto;
		border-left: 4px solid #322F3B;
	}
	.toast-icon {
		display: flex;
		align-items: center;
		flex-shrink: 0;
		width: 20px;
		height: 20px;
	}
	.toast-icon :global(svg) {
		width: 20px;
		height: 20px;
	}
	.toast-msg {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.toast-info {
		background: color-mix(in srgb, var(--md-sys-color-primary) 8%, var(--md-sys-color-secondary-container));
		color: var(--md-sys-color-on-secondary-container);
	}
	.toast-error {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.toast-warning {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.toast-success {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	@keyframes slideIn {
		from {
			transform: translateX(100%);
			opacity: 0;
		}
		to {
			transform: translateX(0);
			opacity: 1;
		}
	}
</style>
