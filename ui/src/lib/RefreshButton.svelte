<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';

	interface Props {
		label?: string;
		loadingLabel?: string;
		loading?: boolean;
		compact?: boolean;
		iconOnly?: boolean;
		size?: 'default' | 'toolbar' | 'dense';
		onclick?: () => void;
		disabled?: boolean;
		title?: string;
		className?: string;
	}

	/**
	 * RefreshButton — shared loading and sizing contract for refresh actions.
	 */
	let {
		label = '刷新',
		loadingLabel = '刷新中…',
		loading = false,
		compact = false,
		iconOnly = false,
		size = 'default',
		onclick,
		disabled = false,
		title = '',
		className = '',
	}: Props = $props();

	let buttonClass = $derived(
		[
			'refresh-button',
			compact ? 'refresh-button--compact' : '',
			loading ? 'refresh-button--loading' : '',
			className,
		]
			.filter(Boolean)
			.join(' '),
	);
	let currentLabel = $derived(loading ? loadingLabel : label);
</script>

{#if iconOnly}
	<MaterialIconButton
		icon="refresh"
		className={buttonClass}
		label={currentLabel}
		title={title || currentLabel}
		ariaBusy={loading}
		{size}
		disabled={disabled || loading}
		{onclick}
	/>
{:else}
	<MaterialButton
		variant="outlined"
		className={buttonClass}
		label={currentLabel}
		ariaLabel={currentLabel}
		ariaBusy={loading}
		disabled={disabled || loading}
		{onclick}
	/>
{/if}

<style>
	:global(.md-btn.refresh-button) {
		min-width: var(--md-comp-refresh-button-width);
		white-space: nowrap;
	}
	:global(.md-btn.refresh-button--compact) {
		min-width: var(--md-comp-refresh-button-compact-width);
	}
	:global(.md-btn.refresh-button--loading) {
		cursor: wait;
	}
	:global(.md-btn.refresh-button--loading::before) {
		content: '';
		width: var(--md-sys-icon-size);
		height: var(--md-sys-icon-size);
		flex: 0 0 auto;
		border: 2px solid currentColor;
		border-top-color: transparent;
		border-radius: var(--md-sys-shape-full);
		animation: refresh-button-spin var(--md-sys-motion-duration-medium) linear infinite;
	}
	:global(.md-icon-btn.refresh-button--loading::before) {
		content: '';
		position: absolute;
		z-index: 2;
		width: 16px;
		height: 16px;
		border: 2px solid currentColor;
		border-top-color: transparent;
		border-radius: var(--md-sys-shape-full);
		animation: refresh-button-spin var(--md-sys-motion-duration-medium) linear infinite;
	}
	:global(.md-icon-btn.refresh-button--loading > svg) {
		opacity: 0;
	}
	@keyframes refresh-button-spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
