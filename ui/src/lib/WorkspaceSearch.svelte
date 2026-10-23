<script lang="ts">
	import Icon from './Icon.svelte';
	import MaterialIconButton from './MaterialIconButton.svelte';

	interface Props {
		value?: string;
		label: string;
		placeholder?: string;
		id?: string;
		describedBy?: string;
		controls?: string;
		disabled?: boolean;
		compact?: boolean;
		onInput?: (value: string) => void;
		onKeydown?: (event: KeyboardEvent) => void;
	}

	/** Shared search control for workspace filters and settings discovery. */
	let {
		value = '',
		label,
		placeholder = label,
		id = undefined,
		describedBy = undefined,
		controls = undefined,
		disabled = false,
		compact = false,
		onInput = () => {},
		onKeydown = undefined,
	}: Props = $props();
	let input: HTMLInputElement;

	function clear() {
		onInput('');
		input?.focus();
	}
</script>

<div class="workspace-search" class:compact>
	<Icon name="search" size={18} />
	<input
		bind:this={input}
		{id}
		type="search"
		{value}
		{placeholder}
		{disabled}
		aria-label={label}
		aria-describedby={describedBy}
		aria-controls={controls}
		autocomplete="off"
		oninput={(event) => onInput(event.currentTarget.value)}
		onkeydown={onKeydown}
	/>
	{#if value}
		<MaterialIconButton
			label={`清空${label}`}
			icon="close"
			variant="ghost"
			size="dense"
			{disabled}
			onclick={clear}
		/>
	{/if}
</div>

<style>
	.workspace-search {
		--search-height: var(--md-comp-toolbar-height);
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-width: 0;
		min-height: var(--search-height);
		padding-inline: var(--md-sys-space-md) var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
	}
	.workspace-search:focus-within {
		border-color: var(--md-sys-color-primary);
		box-shadow: inset 0 0 0 1px var(--md-sys-color-primary);
	}
	.workspace-search.compact {
		--search-height: var(--md-comp-textfield-container-height);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	input {
		flex: 1;
		width: 100%;
		min-width: 0;
		min-height: var(--search-height);
		padding: 0;
		border: 0;
		outline: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
	}
	/* The container owns focus indication; do not stack the global input ring. */
	input:focus-visible {
		outline: none;
		box-shadow: none;
	}
	input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
	}
	input:disabled {
		opacity: 0.38;
	}
	input::-webkit-search-cancel-button {
		-webkit-appearance: none;
	}
</style>
