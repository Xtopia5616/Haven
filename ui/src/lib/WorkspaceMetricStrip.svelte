<script>
	/**
	 * WorkspaceMetricStrip — compact, shared overview metrics for workspace pages.
	 * @prop {Array<{ id?: string; value: string | number; label: string; detail?: string; tone?: string }>} items — metrics to display
	 */
	let { items = [] } = $props();
</script>

{#if items.length > 0}
	<div class="workspace-metrics" role="list" aria-label="工作区概览">
		{#each items as item, index (item.id || `${item.label}-${index}`)}
			<div class="workspace-metric" data-tone={item.tone || 'neutral'} role="listitem">
				<strong class="workspace-metric__value">{item.value}</strong>
				<span class="workspace-metric__label">{item.label}</span>
				{#if item.detail}<span class="workspace-metric__detail">{item.detail}</span>{/if}
			</div>
		{/each}
	</div>
{/if}

<style>
	.workspace-metrics {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 160px), 1fr));
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-xl);
	}
	.workspace-metric {
		display: grid;
		grid-template-columns: auto 1fr;
		align-items: baseline;
		column-gap: var(--md-sys-space-sm);
		row-gap: var(--md-sys-space-2xs);
		min-width: 0;
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.workspace-metric__value {
		grid-row: span 2;
		font-size: var(--md-sys-typescale-headline-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-headline-medium-line-height);
		font-variant-numeric: tabular-nums;
		color: var(--md-sys-color-on-surface);
	}
	.workspace-metric__label {
		min-width: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.workspace-metric__detail {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.workspace-metric[data-tone='running'] .workspace-metric__value {
		color: var(--md-sys-color-success);
	}
	.workspace-metric[data-tone='scheduled'] .workspace-metric__value {
		color: var(--md-sys-color-tertiary);
	}
	.workspace-metric[data-tone='error'] .workspace-metric__value {
		color: var(--md-sys-color-error);
	}
	@media (max-width: 455px) {
		.workspace-metric {
			padding-inline: var(--md-sys-space-md);
		}
		.workspace-metric__value {
			font-size: var(--md-sys-typescale-title-large-size);
			line-height: var(--md-sys-typescale-title-large-line-height);
		}
	}
</style>
