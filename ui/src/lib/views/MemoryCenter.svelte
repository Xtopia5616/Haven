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
	import type { Fact, MemoryRecallFilter, MemoryRecallState } from '$lib/contracts/memory.ts';

	interface Props {
		facts?: Fact[];
		factsLoaded?: boolean;
		factSourceFilter?: MemoryFactSourceFilter;
		factSourceOptions?: Array<{ value: MemoryFactSourceFilter; label: string }>;
		newFact?: { predicate: string; object: string; tags: string };
		addingFact?: boolean;
		memoryRecall: MemoryRecallState;
		onRecallKindChange?: (value: MemoryRecallFilter) => void;
		onRunRecall?: () => void;
		onClearRecall?: () => void;
		onFactSourceFilterChange?: (value: MemoryFactSourceFilter) => void;
		onAddFact?: () => boolean | Promise<boolean>;
		onDeleteFact?: (factId: string) => void;
	}

	let {
		facts = [],
		factsLoaded = false,
		factSourceFilter = '',
		factSourceOptions = [],
		newFact = { predicate: '', object: '', tags: '' },
		addingFact = false,
		memoryRecall,
		onRecallKindChange = () => {},
		onRunRecall = () => {},
		onClearRecall = () => {},
		onFactSourceFilterChange = () => {},
		onAddFact = () => false,
		onDeleteFact = () => {},
	}: Props = $props();

	let addFactDialogOpen = $state(false);

	const memoryScopeOptions = [
		{ value: 'all', label: '全部记忆' },
		{ value: 'fact', label: '关于你的事实' },
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
				bind:value={memoryRecall.query}
				placeholder="搜索事实或过去的对话，例如：深色主题"
				onkeydown={handleKeydown}
				autocomplete="off"
			/>
		</label>
		<div class="memory-center-scope">
			<MaterialSelect
				id="memory-center-scope"
				value={memoryRecall.kind}
				ariaLabel="记忆范围"
				width="compact"
				options={memoryScopeOptions}
				onChange={handleScopeChange}
			/>
		</div>
		{#if !memoryRecall.searched && memoryRecall.kind !== 'episode'}
			<div class="memory-center-source">
				<MaterialSelect
					id="memory-center-source"
					value={factSourceFilter}
					ariaLabel="事实来源"
					width="compact"
					options={factSourceOptions}
					onChange={handleFactSourceFilterInput}
				/>
			</div>
		{/if}
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
			<CountChip count={facts.length} label="条记忆" />
		{/if}
	</div>

	{#if memoryRecall.searched || memoryRecall.kind === 'episode'}
		<MemoryRecall
			{memoryRecall}
			showToolbar={false}
			showHeading={false}
			{onRecallKindChange}
			{onRunRecall}
		/>
	{:else}
		<LongTermFacts {facts} {factsLoaded} {onDeleteFact} />
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
	}
	.memory-center-toolbar {
		align-items: center;
		margin-bottom: 0;
	}
	.memory-center-search {
		flex: 1 1 auto;
		min-width: 0;
	}
	.memory-center-scope {
		flex: 0 1 auto;
	}
	.memory-center-source {
		flex: 0 1 auto;
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
	@media (max-width: 760px) {
		.memory-center-toolbar {
			align-items: stretch;
		}
		.memory-center-search {
			flex-basis: 100%;
		}
		.memory-center-scope,
		.memory-center-source {
			flex: 1 1 0;
			width: auto;
		}
		.memory-center-scope :global(.md-select-container),
		.memory-center-source :global(.md-select-container) {
			width: 100%;
		}
	}
</style>
