<script lang="ts">
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import BuiltinToolRootCard from '$lib/BuiltinToolRootCard.svelte';
	import type {
		BuiltinToolCard as BuiltinToolCardModel,
		BuiltinToolEntry,
	} from './builtinToolPresentation.ts';

	interface Props {
		tool: BuiltinToolCardModel;
		onToggle?: (name: string, checked: boolean) => void;
	}

	let { tool: card, onToggle }: Props = $props();
	let operations = $derived(card.roots.flatMap((root) => root.operations));
	let enabledCount = $derived(operations.filter((operation) => operation.enabled).length);
	let statusLabel = $derived(`${enabledCount}/${operations.length} 个操作已启用`);
	let statusTone: 'error' | 'success' | 'warning' = $derived(
		enabledCount === 0 ? 'error' : enabledCount === operations.length ? 'success' : 'warning',
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
		<div class="expandable-context-card-meta">
			<StatusBadge label={`${card.roots.length} 个根能力`} tone={'neutral' as const} />
			<StatusBadge label={`${operations.length} 个操作`} tone={'neutral' as const} />
			<StatusBadge label={statusLabel} tone={statusTone} />
		</div>
	{/snippet}
	{#snippet children()}
		<p class="expandable-context-card-description">
			展开根能力后可查看具体操作，并分别管理每个操作的启用状态。
		</p>
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
	.root-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	:global(.expandable-context-card[data-card-kind='builtin-family'] .card-body) {
		padding-bottom: var(--md-sys-space-md);
	}
	:global(
		.expandable-context-card[data-card-kind='builtin-family']
			.card-body
			> .root-list
			> .expandable-context-card
	) {
		background: var(--md-sys-color-surface-container-lowest);
	}
	:global(
		.expandable-context-card[data-card-kind='builtin-family']
			.card-body
			> .root-list
			> .expandable-context-card
			.card-header
	) {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	:global(
		.expandable-context-card[data-card-kind='builtin-family']
			.card-body
			> .root-list
			> .expandable-context-card
			.card-body
	) {
		padding-left: var(--md-sys-space-md);
		padding-right: var(--md-sys-space-md);
	}
</style>
