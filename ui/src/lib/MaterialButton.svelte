<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { ControlWidth } from './controlWidth.ts';

	interface Props {
		variant?: 'filled' | 'tonal' | 'elevated' | 'outlined' | 'text' | 'danger';
		width?: ControlWidth;
		label?: string;
		onclick?: (event: MouseEvent) => void;
		disabled?: boolean;
		title?: string;
		ariaLabel?: string;
		ariaBusy?: boolean;
		ariaExpanded?: boolean;
		ariaChecked?: boolean;
		ariaPressed?: boolean;
		role?: string;
		style?: string;
		ariaHaspopup?: 'menu' | 'listbox' | 'tree' | 'grid' | 'dialog' | boolean;
		id?: string;
		className?: string;
		children?: Snippet;
	}

	/**
	 * Material Button — the shared text/action button primitive.
	 */
	let {
		variant = 'outlined',
		width = 'content',
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
	}: Props = $props();
</script>

<button
	{id}
	class="md-btn md-btn--{variant} {className}"
	data-width={width}
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
		onclick?.(event);
	}}
>
	{#if label}
		{label}
	{:else}
		{@render children?.()}
	{/if}
</button>
