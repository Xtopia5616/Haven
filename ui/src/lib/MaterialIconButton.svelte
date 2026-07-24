<script>
	/**
	 * Material Icon Button — small circular icon button.
	 * @prop {string} label — aria-label
	 * @prop {'default'|'danger'|'primary'} variant
	 * @prop {function} onclick
	 */
	let { label = '', variant = 'default', onclick, children } = $props();
</script>

<button
	class="md-icon-btn"
	data-variant={variant}
	aria-label={label}
	onclick={(e) => { e.stopPropagation(); onclick?.(); }}
>
	{@render children?.()}
</button>

<style>
	.md-icon-btn {
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		width: 32px;
		height: 32px;
		border-radius: var(--md-sys-shape-small);
		cursor: pointer;
		font-size: 14px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-icon-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-icon-btn[data-variant='danger']:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.md-icon-btn[data-variant='primary'] {
		color: var(--md-sys-color-primary);
	}
	.md-icon-btn[data-variant='primary']:hover {
		background: var(--md-sys-color-primary-container);
	}
</style>
