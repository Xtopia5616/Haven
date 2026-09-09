<script>
	/**
	 * Shared count chip for workspace filter bars and resource lists.
	 * @prop {number} count
	 * @prop {string} label
	 * @prop {string} prefix
	 * @prop {boolean} live — announce count changes to assistive technology
	 * @prop {string} className — optional host-specific class
	 */
	let { count = 0, label = '项', prefix = '共', live = false, className = '' } = $props();

	const displayCount = $derived(
		Number.isFinite(Number(count)) ? Math.max(0, Math.floor(Number(count))) : 0,
	);
</script>

<span class={`count-chip md-chip ${className}`.trim()} aria-live={live ? 'polite' : undefined}
	>{prefix} {displayCount} {label}</span
>
