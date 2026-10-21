<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import {
		isMemoryFactSourceInput,
		isMemoryRecallFilter,
		type MemoryFactSourceFilter,
	} from '$lib/contracts/memory.ts';
	import LongTermFacts from './LongTermFacts.svelte';
	import MemoryRecall from './MemoryRecall.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import PendingFactProposals from './PendingFactProposals.svelte';
	import type {
		Fact,
		FactProposal,
		FactProposalDecision,
		MemoryRecallFilter,
		MemoryRecallState,
	} from '$lib/contracts/memory.ts';

	interface Props {
		facts?: Fact[];
		factsLoaded?: boolean;
		factsHasMore?: boolean;
		factsLoadingMore?: boolean;
		factProposals?: FactProposal[];
		factProposalsLoaded?: boolean;
		factProposalsHasMore?: boolean;
		factProposalsLoadingMore?: boolean;
		busyProposalId?: string | null;
		factSourceFilter?: MemoryFactSourceFilter;
		factSourceOptions?: Array<{ value: MemoryFactSourceFilter; label: string }>;
		newFact?: { predicate: string; object: string; tags: string };
		addingFact?: boolean;
		memoryRecall: MemoryRecallState;
		onRecallKindChange?: (value: MemoryRecallFilter) => void;
		onRecallQueryChange?: (value: string) => void;
		onRunRecall?: () => void;
		onClearRecall?: () => void;
		onFactSourceFilterChange?: (value: MemoryFactSourceFilter) => void;
		onLoadMoreFacts?: () => void;
		onLoadMoreFactProposals?: () => void;
		onReviewFactProposal?: (proposalId: string, decision: FactProposalDecision) => void;
		onAddFact?: () => boolean | Promise<boolean>;
		onDeleteFact?: (factId: string) => void;
	}

	let {
		facts = [],
		factsLoaded = false,
		factsHasMore = false,
		factsLoadingMore = false,
		factProposals = [],
		factProposalsLoaded = false,
		factProposalsHasMore = false,
		factProposalsLoadingMore = false,
		busyProposalId = null,
		factSourceFilter = '',
		factSourceOptions = [],
		newFact = { predicate: '', object: '', tags: '' },
		addingFact = false,
		memoryRecall,
		onRecallKindChange = () => {},
		onRecallQueryChange = () => {},
		onRunRecall = () => {},
		onClearRecall = () => {},
		onFactSourceFilterChange = () => {},
		onLoadMoreFacts = () => {},
		onLoadMoreFactProposals = () => {},
		onReviewFactProposal = () => {},
		onAddFact = () => false,
		onDeleteFact = () => {},
	}: Props = $props();

	let addFactDialogOpen = $state(false);

	const MEMORY_SCOPE_OPTIONS = [
		{ value: 'all', label: '全部记忆' },
		{ value: 'fact', label: '关于你的事实' },
		{ value: 'profile', label: '用户画像' },
		{ value: 'semantic', label: '语义记忆' },
		{ value: 'procedural', label: '可复用流程' },
		{ value: 'episode', label: '过去的对话' },
	];

	function handleScopeChange(value: string) {
		if (isMemoryRecallFilter(value)) {
			onRecallKindChange(value);
			onRunRecall();
		}
	}

	function handleFactSourceFilterInput(value: string) {
		if (value === '' || isMemoryFactSourceInput(value)) {
			onFactSourceFilterChange(value);
		}
	}

	function handleRecallQueryInput(event: Event) {
		const value = (event.currentTarget as HTMLInputElement).value;
		if (value === memoryRecall.query) return;
		onRecallQueryChange(value);
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') onRunRecall();
	}

	async function saveNewFact() {
		const saved = await onAddFact();
		if (saved) addFactDialogOpen = false;
	}
</script>

<div class="memory-center">
	<div class="memory-center-toolbar workspace-filter-bar" role="search">
		<label class="memory-center-search" for="memory-center-query">
			<input
				id="memory-center-query"
				type="search"
				class="md-input"
				aria-label="记忆关键词"
				value={memoryRecall.query}
				placeholder="搜索事实或过去的对话，例如：深色主题"
				oninput={handleRecallQueryInput}
				onkeydown={handleKeydown}
				autocomplete="off"
			/>
		</label>
		<div class="memory-center-controls">
			<div class="memory-center-selects">
				<MaterialSelect
					id="memory-center-scope"
					value={memoryRecall.kind}
					ariaLabel="记忆范围"
					width="compact"
					options={MEMORY_SCOPE_OPTIONS}
					onChange={handleScopeChange}
				/>
				{#if !memoryRecall.searched && ['all', 'fact'].includes(memoryRecall.kind)}
					<MaterialSelect
						id="memory-center-source"
						value={factSourceFilter}
						ariaLabel="事实来源"
						width="compact"
						options={factSourceOptions}
						onChange={handleFactSourceFilterInput}
					/>
				{/if}
			</div>
			<div class="memory-center-actions">
				{#if memoryRecall.searched}
					<MaterialButton variant="text" label="清除" onclick={() => onClearRecall()} />
				{/if}
				<MaterialButton
					variant="outlined"
					label="添加记忆"
					onclick={() => (addFactDialogOpen = true)}
				/>
				{#if memoryRecall.searched && !memoryRecall.loading}
					<CountChip count={memoryRecall.results.length} label="条结果" />
				{:else if factsLoaded}
					<CountChip count={facts.length} label={factsHasMore ? '条已加载' : '条记忆'} />
				{/if}
			</div>
		</div>
	</div>

	{#if memoryRecall.searched || !['all', 'fact'].includes(memoryRecall.kind)}
		<MemoryRecall
			{memoryRecall}
			showToolbar={false}
			showHeading={false}
			{onRecallKindChange}
			{onRunRecall}
		/>
	{:else}
		<PendingFactProposals
			proposals={factProposals}
			loaded={factProposalsLoaded}
			hasMore={factProposalsHasMore}
			loadingMore={factProposalsLoadingMore}
			{busyProposalId}
			onLoadMore={onLoadMoreFactProposals}
			onReview={onReviewFactProposal}
		/>
		<LongTermFacts
			{facts}
			{factsLoaded}
			{factsHasMore}
			{factsLoadingMore}
			{onLoadMoreFacts}
			{onDeleteFact}
		/>
	{/if}
</div>

<MaterialDialog
	open={addFactDialogOpen}
	title="添加长期记忆"
	dialogClass="memory-add-dialog"
	onClose={() => (addFactDialogOpen = false)}
>
	{#snippet children()}
		<div class="memory-fact-form">
			<label for="memory-fact-predicate">谓词</label>
			<input
				id="memory-fact-predicate"
				type="text"
				class="md-input"
				placeholder="例如：喜欢、email"
				bind:value={newFact.predicate}
				autocomplete="off"
			/>
			<label for="memory-fact-object">对象</label>
			<input
				id="memory-fact-object"
				type="text"
				class="md-input"
				placeholder="例如：深色主题"
				bind:value={newFact.object}
				autocomplete="off"
			/>
			<label for="memory-fact-tags">标签 <span>可选</span></label>
			<input
				id="memory-fact-tags"
				type="text"
				class="md-input"
				placeholder="逗号分隔"
				bind:value={newFact.tags}
				autocomplete="off"
			/>
		</div>
	{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="取消"
			disabled={addingFact}
			onclick={() => (addFactDialogOpen = false)}
		/>
		<MaterialButton
			variant="filled"
			label={addingFact ? '保存中…' : '保存记忆'}
			disabled={addingFact || !newFact.predicate.trim() || !newFact.object.trim()}
			onclick={saveNewFact}
		/>
	{/snippet}
</MaterialDialog>

<style>
	.memory-center {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
		min-width: 0;
		container-type: inline-size;
	}
	.memory-center-toolbar {
		align-items: center;
		flex-wrap: nowrap;
		margin-bottom: 0;
	}
	.memory-center-search {
		flex: 1 1 auto;
		min-width: 0;
	}
	.memory-center-controls,
	.memory-center-selects,
	.memory-center-actions {
		display: flex;
		align-items: center;
		gap: var(--md-comp-toolbar-gap);
		min-width: 0;
	}
	.memory-center-controls {
		flex: 0 0 auto;
	}
	.memory-center-selects {
		flex-wrap: wrap;
	}
	.memory-center-actions {
		flex: 0 0 auto;
	}
	.memory-fact-form {
		display: grid;
		gap: var(--md-sys-space-xs);
	}
	.memory-fact-form label {
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.memory-fact-form label span {
		font-weight: 400;
	}
	:global(.memory-add-dialog) {
		width: min(480px, calc(100vw - var(--md-sys-space-2xl)));
	}
	@container (max-width: 760px) {
		.memory-center-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.memory-center-search {
			flex: 0 1 auto;
			width: 100%;
		}
		.memory-center-controls {
			align-items: stretch;
			flex-direction: column;
			width: 100%;
		}
		.memory-center-selects {
			display: grid;
			grid-template-columns: repeat(auto-fit, minmax(min(100%, 150px), 1fr));
			width: 100%;
		}
		.memory-center-selects :global(.md-select-container) {
			flex: 1 1 140px;
			min-width: 0;
			width: 100%;
		}
		.memory-center-actions {
			flex-wrap: wrap;
			width: 100%;
		}
		.memory-center-actions :global(.count-chip) {
			margin-left: auto;
		}
	}
</style>
