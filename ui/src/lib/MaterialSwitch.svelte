<script>
	/**
	 * Material Switch — standardised toggle switch.
	 * @prop {boolean} checked
	 * @prop {function} onChange — (checked: boolean) => void
	 */
	let { checked = false, onChange } = $props();

	/**
	 * @param {any} e
	 */
	function handleChange(e) {
		onChange?.(e.target.checked);
	}
</script>

<label class="md-switch-label">
	<input type="checkbox" class="md-switch-input" {checked} onchange={handleChange} />
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
		width: 52px;
		height: 32px;
		border: 2px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-highest);
		cursor: pointer;
		transition: background var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		flex-shrink: 0;
	}
	.md-switch-track::after {
		content: '';
		position: absolute;
		top: 50%;
		left: 4px;
		width: 20px;
		height: 20px;
		border-radius: var(--md-sys-shape-extra-small);
		background: var(--md-sys-color-outline);
		transform: translateY(-50%);
		transition: left var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-switch-input:checked + .md-switch-track {
		background: var(--md-sys-color-primary);
		border-color: transparent;
	}
	.md-switch-input:checked + .md-switch-track::after {
		left: 26px;
		background: var(--md-sys-color-on-primary);
	}
</style>
