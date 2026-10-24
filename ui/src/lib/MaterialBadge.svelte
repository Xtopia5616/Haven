<script lang="ts">
	interface Props {
		text: string;
		variant?:
			| 'default'
			| 'primary'
			| 'secondary'
			| 'tertiary'
			| 'success'
			| 'warning'
			| 'error';
		size?: 'default' | 'compact';
		shape?: 'small' | 'pill';
		className?: string;
		dataTone?: string;
		title?: string;
	}

	/**
	 * Material Badge — small status / category label.
	 */
	let {
		text,
		variant = 'default',
		size = 'default',
		shape = 'small',
		className = '',
		dataTone = undefined,
		title = undefined,
	}: Props = $props();
	let badgeClass = $derived(
		[
			'md-badge',
			className,
			size === 'compact' ? 'md-badge--compact' : '',
			shape === 'pill' ? 'md-badge--pill' : '',
		]
			.filter(Boolean)
			.join(' '),
	);
</script>

<span class={badgeClass} data-variant={variant} data-tone={dataTone} {title}>
	{text}
</span>

<style>
	.md-badge {
		display: inline-flex;
		align-items: center;
		flex: none;
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-badge--compact {
		padding: 1px 6px;
	}
	.md-badge--pill {
		border-radius: var(--md-sys-shape-full);
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
	.md-badge[data-variant='tertiary'] {
		background: var(--md-sys-color-tertiary-container);
		color: var(--md-sys-color-on-tertiary-container);
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
