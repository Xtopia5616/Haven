<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import { copyText } from '$lib/clipboard.ts';

	/** @typedef {import('./builtinToolPresentation.ts').BuiltinToolEntry} BuiltinToolEntry */
	/** @type {{ tool: any; onToggle?: (name: string, checked: boolean) => void }} */
	let { tool: card, onToggle } = $props();
	let isGroup = $derived(card.kind === 'operation-group');
	/** @type {BuiltinToolEntry[]} */
	let operations = $derived(card.kind === 'operation-group' ? card.operations : [card.tool]);
	/** @type {BuiltinToolEntry} */
	let primaryTool = $derived(card.kind === 'single' ? card.tool : card.operations[0]);
	let enabledCount = $derived(operations.filter((operation) => operation.enabled).length);
	let groupStatusLabel = $derived(
		isGroup
			? `${enabledCount}/${operations.length} 个操作已启用`
			: primaryTool?.enabled
				? '已启用'
				: '已停用',
	);
	let groupStatusTone = $derived(
		isGroup
			? enabledCount === 0
				? 'error'
				: enabledCount === operations.length
					? 'success'
					: 'warning'
			: primaryTool?.enabled
				? 'success'
				: 'error',
	);

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

	/**
	 * @param {string} name
	 * @param {boolean} checked
	 */
	function handleToggle(name, checked) {
		onToggle?.(name, checked);
	}

	function copyCardSchema() {
		const schema = isGroup
			? Object.fromEntries(operations.map((operation) => [operation.name, operation.schema]))
			: primaryTool?.schema;
		copyText(JSON.stringify(schema, null, 2), isGroup ? '全部 Schema' : 'Schema');
	}

	let contextMenuItems = $derived([
		{
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(isGroup ? card.name : primaryTool.name, '名称'),
		},
		{
			id: 'copySchema',
			label: isGroup ? '复制全部 Schema' : '复制 Schema',
			icon: 'copy',
			action: copyCardSchema,
		},
		...(isGroup
			? []
			: [
					primaryTool.enabled
						? {
								id: 'disable',
								label: '禁用',
								icon: 'power',
								action: () => onToggle?.(primaryTool.name, false),
							}
						: {
								id: 'enable',
								label: '启用',
								icon: 'power',
								action: () => onToggle?.(primaryTool.name, true),
							},
				]),
	]);
</script>

<ExpandableContextCard cardKind="builtin-tool" {contextMenuItems} showActions={!isGroup}>
	{#snippet header()}
		<div class="card-name">{isGroup ? card.label : primaryTool.name}</div>
		<div class="card-meta">
			{#if isGroup}
				<StatusBadge label={`${operations.length} 个 operation`} tone="neutral" />
			{:else}
				<StatusBadge
					label={`风险：${riskLabel(primaryTool.risk)}`}
					tone={riskTone(primaryTool.risk)}
				/>
			{/if}
			<StatusBadge label={groupStatusLabel} tone={groupStatusTone} />
		</div>
	{/snippet}
	{#snippet actions()}
		{#if !isGroup}
			<MaterialSwitch
				checked={primaryTool.enabled}
				ariaLabel={`切换工具 ${primaryTool.name}`}
				onChange={(/** @type {boolean} */ checked) =>
					handleToggle(primaryTool.name, checked)}
			/>
		{/if}
	{/snippet}
	{#snippet children()}
		{#if isGroup}
			<p class="desc">展开后可查看并分别管理此能力下的每个 operation。</p>
			<div class="operation-list" aria-label={`${card.label} operation 列表`}>
				{#each operations as operation (operation.name)}
					<article class="operation-item">
						<div class="operation-header">
							<div class="operation-info">
								<div class="operation-name">{operation.name}</div>
								<div class="operation-meta">
									<StatusBadge
										label={`风险：${riskLabel(operation.risk)}`}
										tone={riskTone(operation.risk)}
									/>
									<StatusBadge
										label={operation.enabled ? '已启用' : '已停用'}
										tone={operation.enabled ? 'success' : 'error'}
									/>
								</div>
							</div>
							<div class="operation-actions">
								<MaterialIconButton
									label={`复制 ${operation.name} Schema`}
									icon="copy"
									size="dense"
									onclick={() =>
										copyText(
											JSON.stringify(operation.schema, null, 2),
											'Schema',
										)}
								/>
								<MaterialSwitch
									checked={operation.enabled}
									ariaLabel={`切换工具 ${operation.name}`}
									onChange={(/** @type {boolean} */ checked) =>
										handleToggle(operation.name, checked)}
								/>
							</div>
						</div>
						<p class="operation-desc">{operation.desc || '暂无描述'}</p>
						{#if operation.schema && Object.keys(operation.schema).length > 0}
							<details class="operation-schema">
								<summary>查看输入 Schema</summary>
								<pre>{JSON.stringify(operation.schema, null, 2)}</pre>
							</details>
						{/if}
					</article>
				{/each}
			</div>
		{:else}
			<p class="desc">{primaryTool.desc || '暂无描述'}</p>
			{#if primaryTool.schema && Object.keys(primaryTool.schema).length > 0}
				<h4>输入 Schema</h4>
				<pre>{JSON.stringify(primaryTool.schema, null, 2)}</pre>
			{/if}
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
	.operation-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.operation-item {
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.operation-header,
	.operation-actions,
	.operation-meta {
		display: flex;
		align-items: center;
	}
	.operation-header {
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.operation-info {
		min-width: 0;
	}
	.operation-name {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
		overflow-wrap: anywhere;
	}
	.operation-meta {
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-xs);
		flex-wrap: wrap;
	}
	.operation-actions {
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
	}
	.operation-desc {
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-md) 0 0;
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.operation-schema {
		margin-top: var(--md-sys-space-sm);
	}
	.operation-schema summary {
		color: var(--md-sys-color-primary);
		cursor: pointer;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.operation-schema summary:focus-visible {
		outline: none;
		box-shadow: var(--md-sys-focus-ring);
		border-radius: var(--md-sys-shape-extra-small);
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
