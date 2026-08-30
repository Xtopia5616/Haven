<script>
	let { value = 0, min = undefined, max = undefined, step = 1, onChange, id = undefined } = $props();

	let stepDecimals = $derived(String(step).split('.')[1]?.length ?? 0);

	/**
	 * @param {number} v
	 */
	function roundToStep(v) {
		let factor = 10 ** stepDecimals;
		return Math.round(v * factor) / factor;
	}

	function increment() {
		let next = roundToStep(Number(value) + step);
		if (max !== undefined) next = Math.min(next, Number(max));
		onChange?.(next);
	}

	function decrement() {
		let next = roundToStep(Number(value) - step);
		if (min !== undefined) next = Math.max(next, Number(min));
		onChange?.(next);
	}

	/**
	 * @param {any} e
	 */
	function handleInput(e) {
		let val = e.target.value === '' ? 0 : Number(e.target.value);
		if (min !== undefined) val = Math.max(val, Number(min));
		if (max !== undefined) val = Math.min(val, Number(max));
		onChange?.(roundToStep(val));
	}
</script>

<div class="md-number-field">
	<input
		{id}
		type="number"
		class="md-input"
		{min}
		{max}
		{step}
		value={value}
		oninput={handleInput}
	/>
	<div class="stepper">
		<button class="stepper-btn" onclick={increment} aria-label="Increase">
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M18 15l-6-6-6 6"/>
			</svg>
		</button>
		<button class="stepper-btn" onclick={decrement} aria-label="Decrease">
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M6 9l6 6 6-6"/>
			</svg>
		</button>
	</div>
</div>

<style>
	.md-number-field {
		display: flex;
		align-items: stretch;
		width: 100%;
		position: relative;
	}
	.md-number-field :global(.md-input) {
		border-top-right-radius: 0;
		border-bottom-right-radius: 0;
		border-right: none;
		padding-right: var(--md-sys-space-sm);
		-moz-appearance: textfield;
		appearance: textfield;
	}
	.md-number-field :global(.md-input::-webkit-inner-spin-button),
	.md-number-field :global(.md-input::-webkit-outer-spin-button) {
		-webkit-appearance: none;
		margin: 0;
	}
	.stepper {
		display: flex;
		flex-direction: column;
		border: 1px solid var(--md-sys-color-outline);
		border-left: none;
		border-radius: 0 var(--md-comp-textfield-corner) var(--md-comp-textfield-corner) 0;
		overflow: hidden;
		flex-shrink: 0;
	}
	.stepper-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 50%;
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		cursor: pointer;
		position: relative;
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.stepper-btn:first-child {
		border-bottom: 1px solid var(--md-sys-color-outline);
	}
	.stepper-btn:hover {
		background: var(--md-sys-color-surface-container-high);
	}
	.stepper-btn:active {
		background: var(--md-sys-color-surface-container-highest);
	}
	.stepper-btn svg {
		pointer-events: none;
	}
</style>