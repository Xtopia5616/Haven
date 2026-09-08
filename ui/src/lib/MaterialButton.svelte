<script>
	/**
	 * Material Button — the shared text/action button primitive.
	 * @prop {'filled'|'tonal'|'elevated'|'outlined'|'text'|'danger'} variant
	 * @prop {string} label — optional text content for simple buttons
	 * @prop {function} onclick
	 * @prop {boolean} disabled
	 * @prop {string} title — optional native tooltip
	 * @prop {string} ariaLabel — optional accessible name when content is not enough
	 * @prop {boolean | undefined} ariaBusy — optional busy state for async actions
	 * @prop {boolean | undefined} ariaExpanded — optional expanded state for disclosure actions
	 * @prop {boolean | undefined} ariaChecked — optional checked state for radio-like controls
	 * @prop {boolean | undefined} ariaPressed — optional pressed state for toggle controls
	 * @prop {string | undefined} role — optional semantic role for specialized controls
	 * @prop {string} style — optional inline style for semantic color variants
	 * @prop {string | undefined} ariaHaspopup — optional popup relationship
	 * @prop {string | undefined} id — optional DOM id
	 * @prop {string} className — additional class names for layout/state styling
	 */
	let {
		variant = 'outlined',
		label = '',
		onclick,
		disabled = false,
		title = '',
		ariaLabel = '',
		ariaBusy = undefined,
		ariaExpanded = undefined,
		ariaChecked = undefined,
		ariaPressed = undefined,
		role = undefined,
		style = '',
		ariaHaspopup = undefined,
		id = undefined,
		className = '',
		children = undefined,
	} = $props();
</script>

<button
	{id}
	class="md-btn md-btn--{variant} {className}"
	aria-label={ariaLabel || undefined}
	aria-busy={ariaBusy === undefined ? undefined : ariaBusy}
	aria-expanded={ariaExpanded === undefined ? undefined : ariaExpanded}
	aria-checked={ariaChecked === undefined ? undefined : ariaChecked}
	aria-pressed={ariaPressed === undefined ? undefined : ariaPressed}
	aria-haspopup={ariaHaspopup}
	{role}
	style={style || undefined}
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
