<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import Icon from '$lib/Icon.svelte';
	import { copyText } from '$lib/clipboard.ts';

	/** @typedef {import('./builtinToolPresentation.ts').BuiltinToolRootCard} BuiltinToolRootCard */
	/** @typedef {import('./builtinToolPresentation.ts').BuiltinToolEntry} BuiltinToolEntry */
	/** @type {{ root: BuiltinToolRootCard; onToggle?: (name: string, checked: boolean) => void }} */
	let { root, onToggle } = $props();
	/** @type {BuiltinToolEntry[]} */
	let operations = $derived(root.operations);
	let enabledCount = $derived(operations.filter((operation) => operation.enabled).length);
	let statusLabel = $derived(`${enabledCount}/${operations.length} 个操作已启用`);
	let statusTone = $derived(
		enabledCount === 0
			? 'error'
			: enabledCount === operations.length
				? 'success'
				: 'warning',
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

	/** @param {string} name @param {boolean} checked */
	function handleToggle(name, checked) {
		onToggle?.(name, checked);
	}

	function copyRootSchema() {
		const schema = Object.fromEntries(
			operations.map((operation) => [operation.name, operation.schema]),
		);
		copyText(JSON.stringify(schema, null, 2), '根能力 Schema');
	}

	let contextMenuItems = $derived([
		{
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(root.name, '名称'),
		},
		{
			id: 'copySchema',
			label: '复制全部 Schema',
			icon: 'copy',
			action: copyRootSchema,
		},
	]);
</script>

<ExpandableContextCard cardKind="builtin-root" {contextMenuItems} showActions={false}>
	{#snippet header()}
		<div class="card-title-row">
			<Icon name={root.icon || 'tools'} size={20} />
			<div class="card-title">
				<div class="card-name">{root.label}</div>
				<div class="card-subtitle" title={root.description}>{root.name}</div>
			</div>
		</div>
		<div class="card-meta">
			<StatusBadge label={`${operations.length} 个操作`} tone="neutral" />
			<StatusBadge label={statusLabel} tone={statusTone} />
		</div>
	{/snippet}
	{#snippet children()}
		<div class="operation-list" aria-label={`${root.label} 操作列表`}>
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
								{#if operation.available === false}
									<StatusBadge label="缺少依赖" tone="warning" />
								{/if}
							</div>
						</div>
						<div class="operation-actions">
							<MaterialIconButton
								label={`复制 ${operation.name} Schema`}
								icon="copy"
								size="dense"
								onclick={() =>
									copyText(JSON.stringify(operation.schema, null, 2), 'Schema')}
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
					{#if operation.available === false && operation.availabilityReason}
						<p class="operation-desc">{operation.availabilityReason}</p>
					{/if}
					{#if operation.schema && Object.keys(operation.schema).length > 0}
						<details class="operation-schema">
							<summary>查看输入 Schema</summary>
							<pre>{JSON.stringify(operation.schema, null, 2)}</pre>
						</details>
					{/if}
				</article>
			{/each}
		</div>
	{/snippet}
</ExpandableContextCard>

<style>
	.card-name {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.card-title-row {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
	}
	.card-title-row :global(.icon) {
		color: var(--md-sys-color-primary);
		margin-top: 1px;
	}
	.card-title {
		min-width: 0;
	}
	.card-meta {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		flex-wrap: wrap;
	}
	.card-subtitle {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.operation-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.operation-item {
		/* Keep the third-level operation cards compact and visually consistent. */
		display: flex;
		flex-direction: column;
		min-height: 120px;
		box-sizing: border-box;
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
	:global(.expandable-context-card[data-card-kind='builtin-root'] .card-body pre) {
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
	@media (max-width: 560px) {
		.operation-header {
			align-items: flex-start;
			flex-direction: column;
		}
		.operation-actions {
			align-self: flex-end;
		}
	}
</style>
