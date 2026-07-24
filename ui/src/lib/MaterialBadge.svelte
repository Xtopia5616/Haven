<script>
	/**
	 * Material Badge — small status / category label.
	 * @prop {string} text
	 * @prop {'default'|'primary'|'secondary'|'success'|'warning'|'error'} variant
	 */
	let { text, variant = 'default' } = $props();
</script>

<span class="md-badge" data-variant={variant}>
	{text}
</span>

<style>
	.md-badge {
		display: inline-flex;
		align-items: center;
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.md-badge[data-variant='default'] {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-badge[data-variant='primary'] {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.md-badge[data-variant='secondary'] {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.md-badge[data-variant='success'] {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.md-badge[data-variant='warning'] {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.md-badge[data-variant='error'] {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
</style>
