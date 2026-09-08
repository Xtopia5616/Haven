<script>
	/**
	 * Tool Search — shared compact search field for structured tool results.
	 * @prop {string} value
	 * @prop {function} onInput
	 * @prop {string} placeholder
	 * @prop {string} ariaLabel
	 * @prop {string} className
	 */
	let {
		value = '',
		onInput = () => {},
		placeholder = '筛选…',
		ariaLabel = '筛选',
		className = '',
	} = $props();
</script>

<input
	class="tool-search {className}"
	type="search"
	{value}
	{placeholder}
	aria-label={ariaLabel}
	oninput={(event) => onInput(event.currentTarget.value)}
/>

<style>
	.tool-search {
		width: 100%;
		box-sizing: border-box;
		margin-bottom: var(--md-sys-space-xs);
		padding: 0 var(--md-sys-space-sm);
		height: var(--md-comp-button-small-height);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-size: var(--md-sys-typescale-label-small-size);
		outline: none;
	}

	.tool-search:focus {
		border-color: var(--md-sys-color-primary);
		outline: 2px solid color-mix(in srgb, var(--md-sys-color-primary) 24%, transparent);
		outline-offset: -1px;
	}
</style>
