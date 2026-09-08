<script>
	import MaterialButton from './MaterialButton.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';

	/**
	 * Material Split Button — main action paired with a disclosure action.
	 * @prop {string} label
	 * @prop {'filled'|'tonal'|'elevated'|'outlined'|'text'|'danger'} variant
	 * @prop {boolean} open
	 * @prop {boolean} disabled
	 * @prop {function} onclick
	 * @prop {function} onToggle
	 * @prop {string} ariaLabel
	 * @prop {string} className
	 */
	let {
		label = '',
		variant = 'filled',
		open = false,
		disabled = false,
		onclick = () => {},
		onToggle = () => {},
		ariaLabel = '打开更多操作',
		className = '',
		children = undefined,
	} = $props();
</script>

<div class="md-split-button {className}" class:open>
	<MaterialButton {variant} {label} {disabled} {onclick} />
	<MaterialIconButton
		variant={variant === 'danger' ? 'danger' : 'default'}
		label={ariaLabel}
		ariaExpanded={open}
		disabled={disabled}
		onclick={onToggle}
		className="md-split-button__toggle"
	>
		<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="6 9 12 15 18 9" /></svg>
	</MaterialIconButton>
	{#if children}{@render children()}{/if}
</div>

<style>
	.md-split-button {
		display: inline-flex;
		align-items: stretch;
	}

	.md-split-button :global(.md-btn) {
		border-radius: var(--md-sys-shape-small) 0 0 var(--md-sys-shape-small);
	}

	.md-split-button :global(.md-icon-btn.md-split-button__toggle) {
		width: var(--md-comp-button-small-height);
		height: auto;
		min-height: var(--md-comp-button-small-height);
		min-width: var(--md-comp-button-small-height);
		border-left: 1px solid color-mix(in srgb, currentColor 24%, transparent);
		border-radius: 0 var(--md-sys-shape-small) var(--md-sys-shape-small) 0;
	}

	.md-split-button :global(.md-icon-btn.md-split-button__toggle svg) {
		width: 16px;
		height: 16px;
		transition: transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}

	.md-split-button.open :global(.md-icon-btn.md-split-button__toggle svg) {
		transform: rotate(180deg);
	}
</style>
