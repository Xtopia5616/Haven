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
		onInput?: (value: string) => void;
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
		onInput = () => {},
	}: Props = $props();
	let input: HTMLInputElement;

	function clear() {
		onInput('');
		input?.focus();
	}
</script>

<div class="workspace-search">
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
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-width: 0;
		min-height: var(--md-comp-toolbar-height);
		padding-inline: var(--md-sys-space-md) var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface-variant);
	}
	.workspace-search:focus-within {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 1px;
	}
	input {
		flex: 1;
		width: 100%;
		min-width: 0;
		min-height: var(--md-comp-toolbar-height);
		padding: 0;
		border: 0;
		outline: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font: inherit;
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
