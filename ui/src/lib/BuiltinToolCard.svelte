<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { tool, onToggle } = $props();

	/** @param {boolean} checked */
	function handleToggle(checked) {
		onToggle?.(tool.name, checked);
	}

	let contextMenuItems = $derived([
		{
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(tool.name, '名称'),
		},
		{
			id: 'copySchema',
			label: '复制 Schema',
			icon: 'copy',
			action: () => copyText(JSON.stringify(tool.schema, null, 2), 'Schema'),
		},
		tool.enabled
			? {
					id: 'disable',
					label: '禁用',
					icon: 'power',
					action: () => onToggle?.(tool.name, false),
				}
			: {
					id: 'enable',
					label: '启用',
					icon: 'power',
					action: () => onToggle?.(tool.name, true),
				},
	]);
</script>

<ExpandableContextCard cardKind="builtin-tool" {contextMenuItems}>
	{#snippet header()}
		<div class="card-name">{tool.name}</div>
		<div class="card-meta">
			<span class="risk-badge risk-{tool.risk}">Risk: {tool.risk}</span>
			<span class="enabled-badge" class:enabled={tool.enabled} class:disabled={!tool.enabled}>
				{tool.enabled ? 'Enabled' : 'Disabled'}
			</span>
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch checked={tool.enabled} onChange={handleToggle} />
	{/snippet}
	{#snippet children()}
		<p class="desc">{tool.desc || 'No description'}</p>
		{#if tool.schema && Object.keys(tool.schema).length > 0}
			<h4>Input Schema</h4>
			<pre>{JSON.stringify(tool.schema, null, 2)}</pre>
		{/if}
	{/snippet}
</ExpandableContextCard>

<style>
	.card-name {
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-xs);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.card-meta {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		flex-wrap: wrap;
	}
	.risk-badge,
	.enabled-badge {
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 700;
	}
	.risk-badge.risk-safe {
		background: var(--md-sys-color-primary-container, #d2e3fc);
		color: var(--md-sys-color-on-primary-container, #001d36);
	}
	.risk-badge.risk-low {
		background: var(--md-sys-color-tertiary-container, #cbe9f0);
		color: var(--md-sys-color-on-tertiary-container, #001f25);
	}
	.risk-badge.risk-medium {
		background: var(--md-sys-color-secondary-container, #d9e3f3);
		color: var(--md-sys-color-on-secondary-container, #0e1d31);
	}
	.risk-badge.risk-high {
		background: #ffd9d4;
		color: #410002;
	}
	.risk-badge.risk-critical {
		background: #93000a;
		color: #ffffff;
	}
	.risk-badge.risk-unknown {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.enabled-badge.enabled {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.enabled-badge.disabled {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.desc {
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-md) 0;
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	:global(.expandable-context-card[data-card-kind='builtin-tool'] .card-body pre) {
		margin-top: var(--md-sys-space-xs);
		padding: var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-highest);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
		overflow-x: auto;
		max-height: 200px;
	}
</style>
