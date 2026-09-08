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
		min-height: var(--md-comp-button-small-height);
		padding: 0 var(--md-sys-spacing-3);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: var(--md-sys-typescale-label-small-weight);
		line-height: 1;
		white-space: nowrap;
	}

	.status-badge[data-tone='info'] {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}

	.status-badge[data-tone='success'] {
		border-color: var(--md-sys-color-tertiary);
		background: var(--md-sys-color-tertiary-container);
		color: var(--md-sys-color-on-tertiary-container);
	}

	.status-badge[data-tone='warning'] {
		border-color: var(--md-sys-color-warning);
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}

	.status-badge[data-tone='error'] {
		border-color: var(--md-sys-color-error);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
</style>
