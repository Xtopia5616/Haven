<script>
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import MaterialIconButton from './MaterialIconButton.svelte';

	/**
	 * Material Dialog — overlay + dialog container.
	 * @prop {boolean} open
	 * @prop {function} onClose
	 * @prop {string} title
	 * @prop {function} onConfirm — optional confirm callback
	 * @prop {string} dialogClass — extra class for the dialog container
	 */
	let {
		open = false,
		onClose,
		title = '',
		children,
		footer = undefined,
		dialogClass = '',
	} = $props();

	/**
	 * @param {MouseEvent} e
	 */
	function handleOverlayClick(e) {
		if (e.target === e.currentTarget) onClose?.();
	}

	/**
	 * @param {KeyboardEvent} e
	 */
	function handleKeydown(e) {
		if (open && e.key === 'Escape') onClose?.();
	}

	/**
	 * @param {KeyboardEvent} e
	 */
	function handleOverlayKeydown(e) {
		if (e.key === 'Escape') onClose?.();
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- Keep the overlay in the logical tree: Svelte's delegated events and
	 teardown must continue to follow the component that owns the dialog. -->
{#if open}
	<div
		class="md-dialog-overlay"
		onclick={handleOverlayClick}
		onkeydown={handleOverlayKeydown}
		role="dialog"
		aria-modal="true"
		aria-labelledby={title ? 'md-dialog-title' : undefined}
		tabindex={-1}
		in:fade={{ duration: 300, easing: cubicOut }}
	>
		<div
			class="md-dialog {dialogClass}"
			role="presentation"
			onclick={(e) => e.stopPropagation()}
			in:scale={{ start: 0.92, duration: 450, easing: cubicOut }}
		>
			{#if title}
				<div class="md-dialog-header">
					<h3 id="md-dialog-title">{title}</h3>
					<MaterialIconButton
						icon="close"
						label="关闭"
						className="md-dialog-close"
						onclick={onClose}
					/>
				</div>
			{/if}
			<div class="md-dialog-body">
				{@render children?.()}
			</div>
			{#if footer}
				<div class="md-dialog-footer">
					{@render footer()}
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.md-dialog-overlay {
		position: fixed;
		inset: 0;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 60%, transparent);
		backdrop-filter: blur(6px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: var(--md-sys-z-dialog);
		isolation: isolate;
	}
	.md-dialog {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		width: 480px;
		max-width: 90vw;
		box-shadow: var(--md-sys-elevation-4);
	}
	.md-dialog-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.md-dialog-header h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	:global(.md-icon-btn.md-dialog-close) {
		background: transparent;
		border-color: transparent;
		color: var(--md-sys-color-on-surface-variant);
		border-radius: var(--md-sys-shape-small);
		width: var(--md-comp-icon-button-compact-size);
		height: var(--md-comp-icon-button-compact-size);
	}
	:global(.md-icon-btn.md-dialog-close:hover) {
		color: var(--md-sys-color-on-surface);
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
	}
	.md-dialog-body {
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
	}
	.md-dialog-footer {
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md) var(--md-sys-space-xl);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
</style>
