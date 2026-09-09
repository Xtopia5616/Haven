<script>
	import { onMount, onDestroy, tick } from 'svelte';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { notificationStore } from './stores.ts';
	import { getStatusColorTokens } from './statusColors.ts';
	import Icon from './Icon.svelte';
	// The container stays mounted (empty when idle) and items are only
	// populated after its first render: toasts inserted into a freshly
	// created each block play no intro transition (the block effect has not
	// run a reaction yet), which made the first toast appear instantly.
	/** @type {any[]} */
	let items = $state([]);
	let mounted = $state(false);
	/** @type {(() => void) | null} */
	let unsub = null;
	onMount(async () => {
		mounted = true;
		await tick();
		unsub = notificationStore.subscribe((v) => (items = v));
	});
	onDestroy(() => unsub?.());

	/** @param {string | undefined} type */
	function getToastStyle(type) {
		const { dot, background, foreground } = getStatusColorTokens(type || 'info');
		return `--toast-accent: ${dot}; --toast-background: ${background}; --toast-foreground: ${foreground};`;
	}
</script>

{#if mounted}
	<div class="toast-container">
		{#each items as item (item.id)}
			<div
				class="toast toast-{item.type || 'info'}"
				style={getToastStyle(item.type)}
				role={item.type === 'error' ? 'alert' : 'status'}
				aria-live={item.type === 'error' ? 'assertive' : 'polite'}
				aria-label={item.msg}
				in:fly={{ x: '100%', duration: 450, easing: cubicOut }}
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
				<span class="toast-msg">{item.msg}</span>
			</div>
		{/each}
	</div>
{/if}

<style>
	.toast-container {
		position: fixed;
		bottom: 80px;
		right: var(--md-sys-content-gutter);
		z-index: var(--md-sys-z-toast);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.toast {
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		padding-left: calc(var(--md-sys-space-lg) + 3px);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-small-line-height);
		box-shadow: var(--md-sys-elevation-2);
		width: min(320px, calc(100vw - 2 * var(--md-sys-content-gutter)));
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		pointer-events: auto;
		border-left: 3px solid var(--toast-accent);
		background: var(--toast-background);
		color: var(--toast-foreground);
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
		overflow-wrap: anywhere;
		white-space: normal;
	}
	.toast-error {
		font-weight: 700;
	}
</style>
