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

<label class="md-switch-label" class:disabled>
	<input
		type="checkbox"
		class="md-switch-input"
		{checked}
		aria-label={ariaLabel || undefined}
		{disabled}
		onchange={handleChange}
	/>
	<span class="md-switch-track">
		<svg
			class="md-switch-icon"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="3"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"><path d="m5 12 4 4L19 7" /></svg
		>
	</span>
</label>

<style>
	.md-switch-label {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		inline-size: var(--md-comp-switch-width);
		min-block-size: var(--md-comp-button-touch-height);
		cursor: pointer;
		user-select: none;
		color: var(--md-sys-color-on-surface);
	}
	.md-switch-label::before {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		opacity: 0;
		pointer-events: none;
		transition: opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-switch-label:hover::before {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.md-switch-label:active::before {
		opacity: var(--md-sys-state-pressed-opacity);
	}
	.md-switch-label.disabled {
		cursor: default;
	}
	.md-switch-label.disabled::before {
		display: none;
	}
	.md-switch-input {
		position: absolute;
		opacity: 0;
		inline-size: 1px;
		block-size: 1px;
		pointer-events: none;
	}
	.md-switch-track {
		position: relative;
		display: inline-block;
		z-index: 1;
		box-sizing: border-box;
		width: var(--md-comp-switch-width);
		height: var(--md-comp-switch-height);
		border: 2px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-outline);
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		flex-shrink: 0;
	}
	.md-switch-track::after {
		content: '';
		position: absolute;
		top: 50%;
		left: var(--md-comp-switch-thumb-offset);
		width: var(--md-comp-switch-thumb-size);
		height: var(--md-comp-switch-thumb-size);
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		box-shadow: var(--md-sys-elevation-1);
		transform: translateY(-50%);
		transition:
			left var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.md-switch-input:checked + .md-switch-track {
		background: var(--md-sys-color-primary);
		border-color: transparent;
		color: var(--md-sys-color-on-primary);
	}
	.md-switch-input:checked + .md-switch-track::after {
		left: calc(100% - var(--md-comp-switch-thumb-size));
		background: currentColor;
	}
	.md-switch-icon {
		position: absolute;
		top: 50%;
		left: calc(100% - var(--md-comp-switch-thumb-size) / 2);
		width: var(--md-comp-switch-icon-size);
		height: var(--md-comp-switch-icon-size);
		z-index: 2;
		color: var(--md-sys-color-primary);
		opacity: 0;
		pointer-events: none;
		transform: translate(-50%, -50%) scale(0.7);
		transition:
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized);
	}
	.md-switch-input:checked + .md-switch-track .md-switch-icon {
		opacity: 1;
		transform: translate(-50%, -50%) scale(1);
	}
	.md-switch-input:focus-visible + .md-switch-track {
		box-shadow: var(--md-sys-focus-ring);
	}
	.md-switch-input:disabled + .md-switch-track {
		opacity: 0.38;
		cursor: default;
	}
	@media (prefers-reduced-motion: reduce) {
		.md-switch-label::before,
		.md-switch-track,
		.md-switch-track::after,
		.md-switch-icon {
			transition-duration: 1ms;
		}
	}
</style>
