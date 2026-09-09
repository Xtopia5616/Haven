<script>
	import Icon from './Icon.svelte';
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
		role="switch"
		aria-label={ariaLabel || undefined}
		{disabled}
		onchange={handleChange}
	/>
	<span class="md-switch-track" aria-hidden="true">
		<span class="md-switch-state-layer"></span>
		<Icon name="check" size={16} strokeWidth={2.5} className="md-switch-icon" />
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
	.md-switch-label.disabled {
		cursor: default;
	}
	.md-switch-input {
		position: absolute;
		inset: 0;
		z-index: 3;
		width: 100%;
		height: 100%;
		margin: 0;
		opacity: 0;
		cursor: inherit;
		appearance: none;
		outline: none;
	}
	.md-switch-track {
		position: relative;
		display: inline-block;
		z-index: 1;
		box-sizing: border-box;
		width: var(--md-comp-switch-width);
		height: var(--md-comp-switch-height);
		border-radius: var(--md-sys-shape-full);
		background: transparent;
		color: var(--md-sys-color-outline);
		flex-shrink: 0;
		pointer-events: none;
	}
	.md-switch-track::before {
		content: '';
		position: absolute;
		inset: 0;
		box-sizing: border-box;
		border: 2px solid var(--md-sys-color-outline);
		border-radius: inherit;
		background: var(--md-sys-color-surface-container-highest);
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-switch-state-layer {
		position: absolute;
		z-index: 1;
		top: 50%;
		left: calc(
			var(--md-comp-switch-thumb-offset) + var(--md-comp-switch-thumb-size) / 2 -
				var(--md-comp-switch-state-layer-size) / 2
		);
		width: var(--md-comp-switch-state-layer-size);
		height: var(--md-comp-switch-state-layer-size);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-on-surface);
		opacity: 0;
		transform: translateY(-50%);
		transition:
			left var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized),
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-switch-label:hover .md-switch-state-layer {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.md-switch-label:active .md-switch-state-layer {
		opacity: var(--md-sys-state-pressed-opacity);
	}
	.md-switch-track::after {
		content: '';
		position: absolute;
		z-index: 2;
		top: 50%;
		left: var(--md-comp-switch-thumb-offset);
		width: var(--md-comp-switch-thumb-size);
		height: var(--md-comp-switch-thumb-size);
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		transform: translateY(-50%);
		transition:
			left var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized),
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-switch-input:checked + .md-switch-track::before {
		background: var(--md-sys-color-primary);
		border-color: transparent;
	}
	.md-switch-input:checked + .md-switch-track {
		color: var(--md-sys-color-on-primary);
	}
	.md-switch-input:checked + .md-switch-track::after {
		left: var(--md-comp-switch-thumb-selected-offset);
		width: var(--md-comp-switch-thumb-selected-size);
		height: var(--md-comp-switch-thumb-selected-size);
		background: currentColor;
	}
	.md-switch-input:checked + .md-switch-track .md-switch-state-layer {
		left: calc(
			var(--md-comp-switch-thumb-selected-offset) +
				var(--md-comp-switch-thumb-selected-size) / 2 -
				var(--md-comp-switch-state-layer-size) / 2
		);
		background: var(--md-sys-color-primary);
	}
	:global(.md-switch-icon) {
		position: absolute;
		top: 50%;
		left: calc(
			var(--md-comp-switch-thumb-selected-offset) +
				var(--md-comp-switch-thumb-selected-size) / 2
		);
		width: var(--md-comp-switch-icon-size);
		height: var(--md-comp-switch-icon-size);
		z-index: 3;
		color: var(--md-sys-color-on-primary-container);
		opacity: 0;
		pointer-events: none;
		transform: translate(-50%, -50%) scale(0.7);
		transition:
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized);
	}
	.md-switch-input:checked + .md-switch-track :global(.md-switch-icon) {
		opacity: 1;
		transform: translate(-50%, -50%) scale(1);
	}
	.md-switch-input:not(:disabled):active + .md-switch-track::after {
		left: calc(
			var(--md-comp-switch-thumb-offset) -
				(var(--md-comp-switch-thumb-pressed-size) - var(--md-comp-switch-thumb-size)) / 2
		);
		width: var(--md-comp-switch-thumb-pressed-size);
		height: var(--md-comp-switch-thumb-pressed-size);
	}
	.md-switch-input:checked:not(:disabled):active + .md-switch-track::after {
		left: calc(
			var(--md-comp-switch-thumb-selected-offset) -
				(
					var(--md-comp-switch-thumb-pressed-size) -
						var(--md-comp-switch-thumb-selected-size)
				) /
				2
		);
	}
	.md-switch-input:focus-visible + .md-switch-track {
		box-shadow: var(--md-sys-focus-ring);
	}
	.md-switch-input:disabled + .md-switch-track {
		opacity: 0.38;
		cursor: default;
	}
	@media (prefers-reduced-motion: reduce) {
		.md-switch-track,
		.md-switch-track::before,
		.md-switch-state-layer,
		.md-switch-track::after,
		:global(.md-switch-icon) {
			transition-duration: 1ms;
		}
	}
</style>
