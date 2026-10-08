<script lang="ts">
	import MaterialNumberField from './MaterialNumberField.svelte';
	import type { ComponentProps } from 'svelte';
	import type { ControlWidth } from './controlWidth.ts';

	interface Props extends Omit<ComponentProps<typeof MaterialNumberField>, 'width'> {
		unit?: string;
		className?: string;
		/** Width of the composite control container. */
		width?: ControlWidth;
	}

	/**
	 * Number input with a consistently aligned unit label.
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
		width = 'fill',
	}: Props = $props();
</script>

<div class="md-number-field-with-unit {className}" data-width={width}>
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
</style>
