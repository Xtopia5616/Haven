<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		label?: string;
		id?: string;
		description?: string;
		stacked?: boolean;
		className?: string;
		children?: Snippet;
	}

	/**
	 * Settings Field — shared label/control row for settings forms.
	 */
	let {
		label = '',
		id = undefined,
		description = '',
		stacked = false,
		className = '',
		children,
	}: Props = $props();
</script>

<div
	class="settings-field settings-field-layout {className}"
	class:settings-field--stacked={stacked}
>
	<div class="settings-field__label">
		{#if id}<label for={id}>{label}</label>{:else}<span>{label}</span>{/if}
		{#if description}<small>{description}</small>{/if}
	</div>
	<div class="settings-field__control">{@render children?.()}</div>
</div>

<style>
	.settings-field {
		min-height: var(--md-comp-list-item-one-line-height);
		padding-block: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}

	.settings-field:last-child {
		border-bottom: 0;
	}

	.settings-field__label {
		min-width: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}

	.settings-field__label label,
	.settings-field__label span {
		display: block;
	}

	.settings-field__label small {
		display: block;
		margin-top: var(--md-sys-space-2xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}

	.settings-field__control {
		min-width: 0;
	}

	.settings-field__control > :global(.md-input),
	.settings-field__control > :global(.md-select-container),
	.settings-field__control > :global(.md-number-field),
	.settings-field__control > :global(.hotkey-input-wrap),
	.settings-field__control > :global(.api-key-field) {
		width: min(100%, var(--md-comp-settings-control-width));
	}

	.settings-field--stacked {
		grid-template-columns: 1fr;
		align-items: start;
	}
</style>
