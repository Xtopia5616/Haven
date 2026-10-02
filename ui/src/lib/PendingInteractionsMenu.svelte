<script lang="ts">
	import Icon from './Icon.svelte';
	import MaterialButton from './MaterialButton.svelte';

	interface PendingInteractionItem {
		id: string;
		kind: 'ask' | 'confirm' | 'scheduled_confirm';
		title: string;
		detail: string;
	}

	interface Props {
		items?: PendingInteractionItem[];
		onSelect?: (id: string) => void;
	}

	let { items = [], onSelect = () => {} }: Props = $props();
	let open = $state(false);

	function select(id: string) {
		open = false;
		onSelect(id);
	}
</script>

{#if items.length > 0}
	<div class="pending-interactions">
		<MaterialButton
			variant="outlined"
			className="pending-interactions-toggle"
			label={`待操作 ${items.length}`}
			ariaExpanded={open}
			ariaHaspopup="menu"
			onclick={() => (open = !open)}
		/>
		{#if open}
			<div class="pending-interactions-menu" role="menu" aria-label="待操作项目">
				{#each items as item (item.id)}
					<button
						class="pending-interaction-item"
						type="button"
						role="menuitem"
						aria-label={`${item.title}：${item.detail}`}
						onclick={() => select(item.id)}
					>
						<Icon name={item.kind === 'ask' ? 'help' : 'alertTriangle'} size={16} />
						<span class="pending-interaction-copy">
							<strong>{item.title}</strong>
							<span>{item.detail}</span>
						</span>
						<Icon name="chevronRight" size={14} />
					</button>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<style>
	.pending-interactions {
		position: relative;
		flex: 0 0 auto;
	}

	:global(.md-btn.pending-interactions-toggle) {
		min-height: var(--md-comp-button-small-height);
		padding-inline: var(--md-sys-space-md);
		border-color: var(--md-sys-color-warning);
		color: var(--md-sys-color-on-warning-container);
		font-size: var(--md-sys-typescale-label-medium-size);
	}

	.pending-interactions-menu {
		position: absolute;
		left: 0;
		bottom: calc(100% + var(--md-sys-space-sm));
		z-index: 5;
		display: grid;
		gap: var(--md-sys-space-2xs);
		width: min(360px, calc(100vw - 2 * var(--md-sys-space-lg)));
		max-height: min(360px, 55vh);
		padding: var(--md-sys-space-xs);
		overflow-y: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-highest);
		box-shadow: var(--md-sys-elevation-3);
	}

	.pending-interaction-item {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 0;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
		color: var(--md-sys-color-on-surface);
		text-align: left;
		cursor: pointer;
	}

	.pending-interaction-item:hover,
	.pending-interaction-item:focus-visible {
		outline: none;
		background: var(--md-sys-color-surface-container-high);
	}

	.pending-interaction-copy {
		display: grid;
		gap: var(--md-sys-space-2xs);
		min-width: 0;
	}

	.pending-interaction-copy strong,
	.pending-interaction-copy > span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pending-interaction-copy strong {
		font-size: var(--md-sys-typescale-body-small-size);
	}

	.pending-interaction-copy > span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
	}
</style>
