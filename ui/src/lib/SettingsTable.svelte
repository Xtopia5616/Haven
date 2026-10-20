<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		ariaLabel: string;
		columns: string;
		header: Snippet;
		children: Snippet;
	}

	let { ariaLabel, columns, header, children }: Props = $props();
</script>

<div
	class="settings-table"
	role="table"
	aria-label={ariaLabel}
	style={`--settings-table-columns: ${columns}`}
>
	<div class="settings-table-header" role="row">
		{@render header()}
	</div>
	<div class="settings-table-body" role="rowgroup">
		{@render children()}
	</div>
</div>

<style>
	.settings-table {
		position: relative;
		min-width: 0;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.settings-table-header,
	:global(.settings-table-row) {
		display: grid;
		grid-template-columns: var(--settings-table-columns);
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.settings-table-header {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium) var(--md-sys-shape-medium) 0 0;
		background: var(--md-sys-color-surface-container-low);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.settings-table-header > :last-child) {
		text-align: right;
	}
	.settings-table-body {
		display: flex;
		flex-direction: column;
	}
	.settings-table-body > :global(* + *) {
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	:global(.settings-table-row) {
		min-width: 0;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	:global(.settings-table-cell) {
		min-width: 0;
	}
	:global(.settings-table-cell-label) {
		display: none;
	}
	:global(.settings-table-actions) {
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-xs);
	}
	@container settings-content (max-width: 640px) {
		.settings-table-header {
			position: absolute;
			top: 0;
			left: 0;
			width: 1px;
			height: 1px;
			padding: 0;
			margin: -1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
			border: 0;
		}
		:global(.settings-table-row) {
			grid-template-columns: 1fr;
			align-items: stretch;
			gap: var(--md-sys-space-sm);
		}
		:global(.settings-table-cell-label) {
			display: block;
			margin-bottom: var(--md-sys-space-xs);
			font-size: var(--md-sys-typescale-label-small-size);
			font-weight: 600;
			line-height: var(--md-sys-typescale-label-small-line-height);
			color: var(--md-sys-color-on-surface-variant);
		}
		:global(.settings-table-actions) {
			justify-content: flex-start;
			flex-wrap: wrap;
		}
		:global(.settings-table-actions > .settings-table-cell-label) {
			flex-basis: 100%;
		}
	}
</style>
