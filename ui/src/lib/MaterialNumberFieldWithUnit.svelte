<script>
	import MaterialNumberField from './MaterialNumberField.svelte';

	/**
	 * Number input with a consistently aligned unit label.
	 * @prop {number} value
	 * @prop {string} unit
	 * @prop {function} onChange
	 * @prop {string | undefined} id
	 * @prop {number | undefined} min
	 * @prop {number | undefined} max
	 * @prop {number} step
	 * @prop {string} className
	 */
	let {
		value = 0,
		unit = '',
		onChange,
		id = undefined,
		min = undefined,
		max = undefined,
		step = 1,
		className = '',
	} = $props();
</script>

<div class="md-number-field-with-unit {className}">
	<MaterialNumberField {value} {id} {min} {max} {step} {onChange} />
	<span class="unit">{unit}</span>
</div>

<style>
	.md-number-field-with-unit {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-width: 0;
	}
	.md-number-field-with-unit :global(.md-number-field) {
		flex: 1 1 auto;
		min-width: 0;
	}
	.unit {
		flex: 0 0 auto;
		min-width: 42px;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.md-number-field-with-unit--limit) {
		width: min(100%, var(--md-comp-settings-control-width));
		flex: 0 1 var(--md-comp-settings-control-width);
	}
</style>
