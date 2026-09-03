<script>
	/**
	 * Material Button — the shared text/action button primitive.
	 * @prop {'filled'|'tonal'|'elevated'|'outlined'|'text'|'danger'} variant
	 * @prop {string} label — optional text content for simple buttons
	 * @prop {function} onclick
	 * @prop {boolean} disabled
	 * @prop {string} title — optional native tooltip
	 * @prop {string} ariaLabel — optional accessible name when content is not enough
	 * @prop {string} className — additional class names for layout/state styling
	 */
	let {
	variant = 'outlined',
	label = '',
	onclick,
		disabled = false,
		title = '',
		ariaLabel = '',
		className = '',
		children,
	} = $props();
</script>

<button
	class="md-btn md-btn--{variant} {className}"
		aria-label={ariaLabel || undefined}
		title={title || undefined}
		{disabled}
		type="button"
		onclick={(event) => {
			event.stopPropagation();
			onclick?.();
		}}
>
	{#if label}
		{label}
	{:else}
		{@render children?.()}
	{/if}
</button>
