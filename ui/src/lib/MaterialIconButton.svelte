<script>
	/**
	 * Material Icon Button — reusable icon button with compact and toolbar sizes.
	 * @prop {string} label — aria-label
	 * @prop {'default'|'ghost'|'danger'|'danger-outline'|'primary'|'tonal'} variant
	 * @prop {'default'|'toolbar'} size — visual size used by the surrounding layout
	 * @prop {'refresh'|'edit'|'delete'|undefined} icon — optional shared action icon
	 * @prop {function} onclick
	 * @prop {boolean} disabled
	 * @prop {boolean | undefined} ariaBusy — optional busy state for async actions
	 * @prop {string} title — optional native tooltip
	 * @prop {string} className — additional class names for layout positioning
	 */
	let {
		label = '',
		variant = 'default',
		size = 'default',
		icon = undefined,
		onclick,
		disabled = false,
		ariaBusy = undefined,
		title = '',
		className = '',
		children = undefined,
	} = $props();
</script>

<button
	class="md-icon-btn {className}"
	data-variant={variant}
	data-size={size}
	aria-label={label}
	aria-busy={ariaBusy === undefined ? undefined : ariaBusy}
	{title}
	{disabled}
	type="button"
	onclick={(e) => {
		e.stopPropagation();
		onclick?.();
	}}
>
	{#if icon === 'refresh'}
		<svg
			width="20"
			height="20"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
			><polyline points="23 4 23 10 17 10" /><path
				d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"
			/></svg
		>
	{:else if icon === 'edit'}
		<svg
			width="20"
			height="20"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
			><path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" /></svg
		>
	{:else if icon === 'delete'}
		<svg
			width="20"
			height="20"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
			><polyline points="3 6 5 6 21 6" /><path
				d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
			/><line x1="10" y1="11" x2="10" y2="17" /><line x1="14" y1="11" x2="14" y2="17" /></svg
		>
	{:else}
		{@render children?.()}
	{/if}
</button>

<style>
	.md-icon-btn {
		position: relative;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		width: var(--md-comp-icon-button-compact-size);
		height: var(--md-comp-icon-button-compact-size);
		min-width: var(--md-comp-icon-button-compact-size);
		min-height: var(--md-comp-icon-button-compact-size);
		flex-shrink: 0;
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
		border-radius: var(--md-comp-button-radius);
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
	.md-icon-btn[data-variant='ghost'] {
		background: transparent;
		border-color: transparent;
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-icon-btn[data-variant='ghost']:hover {
		background: var(--md-sys-color-surface-container-highest);
		border-color: transparent;
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
