<script lang="ts">
	interface Props {
		count?: number;
		label?: string;
		prefix?: string;
		live?: boolean;
		className?: string;
	}

	/**
	 * Shared count chip for workspace filter bars and resource lists.
	 * Announces count changes when requested for assistive technology.
	 */
	let { count = 0, label = '项', prefix = '共', live = false, className = '' }: Props = $props();

	const displayCount = $derived(Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0);
</script>

<span class={`count-chip md-chip ${className}`.trim()} aria-live={live ? 'polite' : undefined}
	>{prefix} {displayCount} {label}</span
>
