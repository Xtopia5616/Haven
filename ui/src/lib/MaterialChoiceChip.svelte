<script>
	/**
	 * Material Choice Chip — compact single-select control.
	 * @prop {string} label
	 * @prop {boolean} selected
	 * @prop {function} onSelect
	 * @prop {function} onKeydown
	 * @prop {boolean} disabled
	 * @prop {string} className
	 */
	let {
		label = '',
		selected = false,
		onSelect = () => {},
		onKeydown = undefined,
		disabled = false,
		className = '',
	} = $props();

	/** @param {MouseEvent} event */
	function handleClick(event) {
		event.stopPropagation();
		onSelect?.();
	}
</script>

<button
	type="button"
	class="md-choice-chip {className}"
	class:selected
	aria-pressed={selected}
	{disabled}
	onclick={handleClick}
	onkeydown={onKeydown}
>
	{label}
</button>

<style>
	.md-choice-chip {
		position: relative;
		height: var(--md-comp-button-small-height);
		min-width: var(--md-comp-button-small-height);
		padding: 0 var(--md-sys-spacing-4);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface);
		color: var(--md-sys-color-on-surface-variant);
		font: inherit;
		font-size: var(--md-sys-typescale-label-large-size);
		font-weight: var(--md-sys-typescale-label-large-weight);
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}

	.md-choice-chip::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: inherit;
		background: currentColor;
		opacity: 0;
		pointer-events: none;
	}

	.md-choice-chip:hover::after {
		opacity: var(--md-state-hover-opacity);
	}

	.md-choice-chip:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 2px;
	}

	.md-choice-chip.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}

	.md-choice-chip:disabled {
		border-color: var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
		cursor: not-allowed;
		opacity: var(--md-state-disabled-opacity);
	}
</style>
