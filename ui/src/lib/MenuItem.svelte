<script>
	/**
	 * Menu Item — shared row primitive for popup and context menus.
	 * @prop {boolean} selected
	 * @prop {boolean} disabled
	 * @prop {boolean} danger
	 * @prop {string | undefined} role
	 * @prop {boolean | undefined} ariaChecked
	 * @prop {string} label
	 * @prop {string} className
	 * @prop {function} onSelect
	 */
	let {
		selected = false,
		disabled = false,
		danger = false,
		role = 'menuitem',
		ariaChecked = undefined,
		label = '',
		className = '',
		onSelect = () => {},
		children = undefined,
	} = $props();
</script>

<button
	type="button"
	class="menu-item {className}"
	class:selected
	class:danger
	{disabled}
	{role}
	aria-checked={ariaChecked === undefined ? undefined : ariaChecked}
	onclick={(event) => {
		event.stopPropagation();
		onSelect(event);
	}}
>
	{#if label}{label}{:else}{@render children?.()}{/if}
</button>

<style>
	.menu-item {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-height: var(--md-comp-button-small-height);
		padding: 0 var(--md-sys-space-md);
		border: 0;
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		text-align: left;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}

	.menu-item:hover:not(:disabled) {
		background: var(--md-sys-color-surface-container-highest);
	}

	.menu-item:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: -2px;
	}

	.menu-item.selected {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}

	.menu-item.danger {
		color: var(--md-sys-color-error);
	}

	.menu-item.danger:hover:not(:disabled) {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}

	.menu-item:disabled {
		cursor: not-allowed;
		opacity: var(--md-state-disabled-opacity);
	}
</style>
