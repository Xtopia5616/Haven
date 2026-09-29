<script>
	import { cubicOut } from 'svelte/easing';
	import { fade, scale } from 'svelte/transition';
	import MaterialIconButton from './MaterialIconButton.svelte';

	/**
	 * Material Dialog — overlay + dialog container.
	 * @prop {boolean} open
	 * @prop {function} onClose
	 * @prop {function} [onEscape] — optional Escape-key behavior; defaults to onClose
	 * @prop {string} title
	 * @prop {any} header — optional custom header snippet
	 * @prop {any} children — dialog body snippet
	 * @prop {any} footer — optional footer snippet
	 * @prop {string} dialogClass — extra class for the dialog container
	 * @prop {string} overlayClass — extra class for the overlay
	 * @prop {string} ariaLabelledby — id for a custom heading
	 * @prop {string} ariaDescribedby — id for descriptive dialog content
	 * @prop {HTMLDivElement | null} dialogElement — bindable panel element for focus management
	 */
	let {
		open = false,
		onClose,
		onEscape = undefined,
		title = '',
		header = undefined,
		children,
		footer = undefined,
		dialogClass = '',
		overlayClass = '',
		ariaLabelledby = undefined,
		ariaDescribedby = undefined,
		dialogElement = $bindable(null),
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
		if (open && e.key === 'Escape') {
			e.preventDefault();
			(onEscape || onClose)?.();
		}
	}

	/**
	 * @param {KeyboardEvent} e
	 */
	function handleOverlayKeydown(e) {
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			(onEscape || onClose)?.();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- Keep the overlay in the logical tree: Svelte's delegated events and
	 teardown must continue to follow the component that owns the dialog. -->
{#if open}
	<div
		class="md-dialog-overlay {overlayClass}"
		onclick={handleOverlayClick}
		onkeydown={handleOverlayKeydown}
		role="dialog"
		aria-modal="true"
		aria-labelledby={ariaLabelledby || (title ? 'md-dialog-title' : undefined)}
		aria-describedby={ariaDescribedby}
		tabindex={-1}
		transition:fade|global={{ duration: 300, easing: cubicOut }}
	>
		<div
			class="md-dialog {dialogClass}"
			role="presentation"
			tabindex="-1"
			bind:this={dialogElement}
			onclick={(e) => e.stopPropagation()}
			transition:scale|global={{
				start: 0.92,
				duration: 300,
				easing: cubicOut,
			}}
		>
			{#if header}
				{@render header()}
			{:else if title}
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
