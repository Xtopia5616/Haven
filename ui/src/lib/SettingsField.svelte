<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { ControlWidth } from './controlWidth.ts';

	interface Props {
		label?: string;
		id?: string;
		description?: string;
		stacked?: boolean;
		controlWidth?: Exclude<ControlWidth, 'equal'>;
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
		controlWidth = 'standard',
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
	<div class="settings-field__control" data-width={controlWidth}>{@render children?.()}</div>
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
		text-align: left;
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
		display: flex;
		align-items: center;
		justify-content: flex-end;
		justify-self: end;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
		width: min(100%, var(--md-comp-settings-control-width));
		max-width: 100%;
		min-width: 0;
	}
	.settings-field__control[data-width='content'] {
		width: fit-content;
	}
	.settings-field__control[data-width='compact'] {
		width: min(100%, var(--md-comp-control-compact-width));
	}
	.settings-field__control[data-width='fill'] {
		width: 100%;
	}
	.settings-field__control > :global(.md-btn[data-width='content']) {
		width: min(100%, var(--md-comp-control-compact-width));
	}

	.settings-field__control[data-width='standard'] > :global(.md-input),
	.settings-field__control[data-width='standard'] > :global(.md-select-container),
	.settings-field__control[data-width='standard'] > :global(.md-number-field),
	.settings-field__control[data-width='standard'] > :global(.md-number-field-with-unit),
	.settings-field__control[data-width='standard'] > :global(.hotkey-input-wrap),
	.settings-field__control[data-width='standard'] > :global(.api-key-field),
	.settings-field__control[data-width='standard'] > :global(.ma-root) {
		width: min(100%, var(--md-comp-settings-control-width));
	}
	.settings-field__control[data-width='content'] > :global(.md-input),
	.settings-field__control[data-width='content'] > :global(.md-select-container),
	.settings-field__control[data-width='content'] > :global(.md-number-field),
	.settings-field__control[data-width='content'] > :global(.md-number-field-with-unit),
	.settings-field__control[data-width='content'] > :global(.hotkey-input-wrap),
	.settings-field__control[data-width='content'] > :global(.api-key-field),
	.settings-field__control[data-width='content'] > :global(.ma-root) {
		width: fit-content;
		max-width: 100%;
	}
	.settings-field__control[data-width='compact'] > :global(.md-input),
	.settings-field__control[data-width='compact'] > :global(.md-select-container),
	.settings-field__control[data-width='compact'] > :global(.md-number-field),
	.settings-field__control[data-width='compact'] > :global(.md-number-field-with-unit),
	.settings-field__control[data-width='compact'] > :global(.hotkey-input-wrap),
	.settings-field__control[data-width='compact'] > :global(.api-key-field),
	.settings-field__control[data-width='compact'] > :global(.ma-root) {
		width: min(100%, var(--md-comp-control-compact-width));
		max-width: 100%;
	}
	.settings-field__control[data-width='fill'] > :global(.md-input),
	.settings-field__control[data-width='fill'] > :global(.md-select-container),
	.settings-field__control[data-width='fill'] > :global(.md-number-field),
	.settings-field__control[data-width='fill'] > :global(.md-number-field-with-unit),
	.settings-field__control[data-width='fill'] > :global(.hotkey-input-wrap),
	.settings-field__control[data-width='fill'] > :global(.api-key-field),
	.settings-field__control[data-width='fill'] > :global(.ma-root) {
		width: 100%;
	}

	.settings-field--stacked {
		grid-template-columns: 1fr;
		align-items: start;
	}
</style>
