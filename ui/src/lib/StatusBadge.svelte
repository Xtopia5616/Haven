<script lang="ts">
	import type { StatusTone } from './statusColors.ts';

	type StatusBadgeTone = Extract<StatusTone, 'neutral' | 'info' | 'success' | 'warning' | 'error'>;

	interface Props {
		label?: string;
		tone?: StatusBadgeTone;
		className?: string;
	}

	/**
	 * Status Badge — shared compact semantic status indicator.
	 */
	let { label = '', tone = 'neutral', className = '' }: Props = $props();
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
