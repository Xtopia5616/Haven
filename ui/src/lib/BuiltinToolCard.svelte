<script>
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import BuiltinToolRootCard from '$lib/BuiltinToolRootCard.svelte';

	/** @typedef {import('./builtinToolPresentation.ts').BuiltinToolCard} BuiltinToolCard */
	/** @typedef {import('./builtinToolPresentation.ts').BuiltinToolEntry} BuiltinToolEntry */
	/** @type {{ tool: BuiltinToolCard; onToggle?: (name: string, checked: boolean) => void }} */
	let { tool: card, onToggle } = $props();
	/** @type {BuiltinToolEntry[]} */
	let operations = $derived(card.roots.flatMap((root) => root.operations));
	let enabledCount = $derived(operations.filter((operation) => operation.enabled).length);
	let statusLabel = $derived(`${enabledCount}/${operations.length} 个操作已启用`);
	let statusTone = $derived(
		enabledCount === 0
			? 'error'
			: enabledCount === operations.length
				? 'success'
				: 'warning',
	);

	function copyFamilySchema() {
		const schema = Object.fromEntries(
			operations.map((operation) => [operation.name, operation.schema]),
		);
		copyText(JSON.stringify(schema, null, 2), '能力族 Schema');
	}

	let contextMenuItems = $derived([
		{
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(card.name, '名称'),
		},
		{
			id: 'copySchema',
			label: '复制全部 Schema',
			icon: 'copy',
			action: copyFamilySchema,
		},
	]);
</script>

<ExpandableContextCard cardKind="builtin-family" {contextMenuItems} showActions={false}>
	{#snippet header()}
		<div class="card-name">{card.label}</div>
		<div class="card-meta">
			<StatusBadge label={`${card.roots.length} 个根能力`} tone="neutral" />
			<StatusBadge label={`${operations.length} 个操作`} tone="neutral" />
			<StatusBadge label={statusLabel} tone={statusTone} />
		</div>
	{/snippet}
	{#snippet children()}
		<p class="desc">展开根能力后可查看具体操作，并分别管理每个操作的启用状态。</p>
		<div class="root-list" aria-label={`${card.label} 根能力列表`}>
			{#each card.roots as root (root.name)}
				<BuiltinToolRootCard {root} {onToggle} />
			{/each}
		</div>
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
	.root-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.desc {
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-md) 0;
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	:global(.expandable-context-card[data-card-kind='builtin-family'] .card-body) {
		padding-bottom: var(--md-sys-space-md);
	}
	:global(.expandable-context-card[data-card-kind='builtin-family'] .card-body > .root-list > .expandable-context-card) {
		background: var(--md-sys-color-surface-container-lowest);
	}
	:global(.expandable-context-card[data-card-kind='builtin-family'] .card-body > .root-list > .expandable-context-card .card-header) {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	:global(.expandable-context-card[data-card-kind='builtin-family'] .card-body > .root-list > .expandable-context-card .card-body) {
		padding-left: var(--md-sys-space-md);
		padding-right: var(--md-sys-space-md);
	}
</style>
