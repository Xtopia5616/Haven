<script>
	/**
	 * Material Switch — standardised toggle switch.
	 * @prop {boolean} checked
	 * @prop {function} onChange — (checked: boolean) => void
	 * @prop {string} ariaLabel — accessible name for the switch
	 * @prop {boolean} disabled — prevents changes while the control is unavailable
	 */
	let { checked = false, onChange, ariaLabel = '', disabled = false } = $props();

	/**
	 * @param {any} e
	 */
	function handleChange(e) {
		onChange?.(e.target.checked);
	}
</script>

<label class="md-switch-label">
	<input
		type="checkbox"
		class="md-switch-input"
		{checked}
		aria-label={ariaLabel || undefined}
		{disabled}
		onchange={handleChange}
	/>
	<span class="md-switch-track"></span>
</label>

<style>
	.md-switch-label {
		display: inline-flex;
		align-items: center;
		cursor: pointer;
		user-select: none;
	}
	.md-switch-input {
		position: absolute;
		opacity: 0;
		width: 0;
		height: 0;
		pointer-events: none;
	}
	.md-switch-track {
		position: relative;
		display: inline-block;
		width: var(--md-comp-switch-width);
		height: var(--md-comp-switch-height);
		border: 2px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-highest);
		cursor: pointer;
		transition:
			background var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		flex-shrink: 0;
	}
	.md-switch-track::after {
		content: '';
		position: absolute;
		top: 50%;
		left: 6px;
		width: var(--md-comp-switch-thumb-size);
		height: var(--md-comp-switch-thumb-size);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-outline);
		transform: translateY(-50%);
		transition:
			left var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.md-switch-input:checked + .md-switch-track {
		background: var(--md-sys-color-primary);
		border-color: transparent;
	}
	.md-switch-input:checked + .md-switch-track::after {
		left: 24px;
		width: var(--md-comp-switch-thumb-selected-size);
		height: var(--md-comp-switch-thumb-selected-size);
		background: var(--md-sys-color-on-primary);
	}
	.md-switch-input:focus-visible + .md-switch-track {
		box-shadow: var(--md-sys-focus-ring);
	}
	.md-switch-input:disabled + .md-switch-track {
		opacity: 0.38;
		cursor: default;
	}
</style>
