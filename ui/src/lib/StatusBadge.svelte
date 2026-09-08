<script>
	/**
	 * Status Badge — shared compact semantic status indicator.
	 * @prop {string} label
	 * @prop {'neutral'|'info'|'success'|'warning'|'error'} tone
	 * @prop {string} className
	 */
	let { label = '', tone = 'neutral', className = '' } = $props();
</script>

<span class="status-badge {className}" data-tone={tone}>{label}</span>

<style>
	.status-badge {
		display: inline-flex;
		align-items: center;
		flex: none;
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
	}

	.status-badge[data-tone='info'] {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}

	.status-badge[data-tone='success'] {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}

	.status-badge[data-tone='warning'] {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}

	.status-badge[data-tone='error'] {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
</style>
