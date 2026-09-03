<script>
	/**
	 * Material Icon Button — reusable icon button with compact and toolbar sizes.
	 * @prop {string} label — aria-label
	 * @prop {'default'|'danger'|'danger-outline'|'primary'|'tonal'} variant
	 * @prop {'default'|'toolbar'} size — visual size used by the surrounding layout
	 * @prop {function} onclick
	 * @prop {boolean} disabled
	 * @prop {string} title — optional native tooltip
	 * @prop {string} className — additional class names for layout positioning
	 */
	let {
		label = '',
		variant = 'default',
		size = 'default',
		onclick,
		disabled = false,
		title = '',
		className = '',
		children,
	} = $props();
</script>

<button
	class="md-icon-btn {className}"
	data-variant={variant}
	data-size={size}
	aria-label={label}
	{title}
	{disabled}
	type="button"
	onclick={(e) => {
		e.stopPropagation();
		onclick?.();
	}}
>
	{@render children?.()}
</button>

<style>
	.md-icon-btn {
		position: relative;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		width: var(--md-comp-icon-button-compact-size);
		height: var(--md-comp-icon-button-compact-size);
		border-radius: var(--md-sys-shape-small);
		cursor: pointer;
		font-size: 14px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		overflow: hidden;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-radius var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-emphasized);
	}
	.md-icon-btn[data-size='toolbar'] {
		width: var(--md-comp-icon-button-size);
		height: var(--md-comp-icon-button-size);
		min-width: var(--md-comp-icon-button-size);
		min-height: var(--md-comp-icon-button-size);
		border-radius: var(--md-sys-shape-small);
	}
	.md-icon-btn[data-size='toolbar']::after {
		content: '';
		position: absolute;
		inset: 0;
		background: currentColor;
		opacity: 0;
		pointer-events: none;
		transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-icon-btn[data-size='toolbar']:hover::after {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.md-icon-btn[data-size='toolbar']:focus-visible::after {
		opacity: var(--md-sys-state-focus-opacity);
	}
	.md-icon-btn[data-size='toolbar']:active::after {
		opacity: var(--md-sys-state-pressed-opacity);
	}
	:global(.md-icon-btn > svg) {
		position: relative;
		z-index: 1;
		pointer-events: none;
	}
	.md-icon-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-icon-btn[data-variant='primary'] {
		background: var(--md-sys-color-primary);
		border-color: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.md-icon-btn[data-variant='primary']:hover {
		background: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-elevation-1);
	}
	.md-icon-btn[data-variant='tonal'] {
		background: var(--md-sys-color-primary-container);
		border-color: transparent;
		color: var(--md-sys-color-on-primary-container);
	}
	.md-icon-btn[data-variant='tonal']:hover {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.md-icon-btn[data-variant='danger'] {
		background: var(--md-sys-color-error);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
	}
	.md-icon-btn[data-variant='danger']:hover {
		background: var(--md-sys-color-error);
		border-color: var(--md-sys-color-error);
		box-shadow: var(--md-sys-elevation-1);
	}
	.md-icon-btn[data-variant='danger-outline'] {
		background: transparent;
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-error);
	}
	.md-icon-btn[data-variant='danger-outline']:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.md-icon-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
		pointer-events: none;
	}
</style>
