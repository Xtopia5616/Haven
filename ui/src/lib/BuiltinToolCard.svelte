<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { tool, onToggle } = $props();

	/** @param {boolean} checked */
	function handleToggle(checked) {
		onToggle?.(tool.name, checked);
	}

	/** @param {string} risk */
	function riskLabel(risk) {
		return (
			{
				safe: '安全',
				low: '低风险',
				medium: '中风险',
				high: '高风险',
				critical: '严重风险',
				unknown: '风险未知',
			}[risk] || risk
		);
	}

	/** @param {string} risk */
	function riskTone(risk) {
		if (risk === 'safe' || risk === 'low') return 'success';
		if (risk === 'medium') return 'warning';
		if (risk === 'high' || risk === 'critical') return 'error';
		return 'neutral';
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
			<StatusBadge label={`风险：${riskLabel(tool.risk)}`} tone={riskTone(tool.risk)} />
			<StatusBadge
				label={tool.enabled ? '已启用' : '已停用'}
				tone={tool.enabled ? 'success' : 'error'}
			/>
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch
			checked={tool.enabled}
			ariaLabel={`切换工具 ${tool.name}`}
			onChange={handleToggle}
		/>
	{/snippet}
	{#snippet children()}
		<p class="desc">{tool.desc || '暂无描述'}</p>
		{#if tool.schema && Object.keys(tool.schema).length > 0}
			<h4>输入 Schema</h4>
			<pre>{JSON.stringify(tool.schema, null, 2)}</pre>
		{/if}
	{/snippet}
</ExpandableContextCard>

<style>
	.card-name {
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-on-surface);
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
