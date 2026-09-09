<script>
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import LongTermFacts from './LongTermFacts.svelte';
	import MemoryRecall from './MemoryRecall.svelte';
	import CountChip from '$lib/CountChip.svelte';

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
		onAddFact = () => {},
		onDeleteFact = () => {},
	} = $props();

	const memoryScopeOptions = [
		{ value: 'all', label: '全部记忆' },
		{ value: 'fact', label: '关于你的事实' },
		{ value: 'episode', label: '过去的对话' },
	];

	/** @param {string} value */
	function handleScopeChange(value) {
		onRecallKindChange(value);
	}

	/** @param {KeyboardEvent} event */
	function handleKeydown(event) {
		if (event.key === 'Enter') onRunRecall();
	}
</script>

<div class="memory-center">
	<div class="memory-center-toolbar workspace-filter-bar" role="search">
		<label class="memory-center-search" for="memory-center-query">
			<span class="sr-only">搜索记忆</span>
			<input
				id="memory-center-query"
				type="search"
				class="md-input"
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
					options={factSourceOptions}
					onChange={onFactSourceFilterChange}
				/>
			</div>
		{/if}
		<MaterialButton
			variant="filled"
			label={memoryRecall.loading ? '搜索中…' : '搜索'}
			ariaBusy={memoryRecall.loading}
			onclick={() => onRunRecall()}
			disabled={memoryRecall.loading}
		/>
		{#if memoryRecall.searched}
			<MaterialButton variant="text" label="清除" onclick={() => onClearRecall()} />
		{/if}
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
		<LongTermFacts
			{facts}
			{factsLoaded}
			showHeader={false}
			{newFact}
			{addingFact}
			{onAddFact}
			{onDeleteFact}
		/>
	{/if}
</div>

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
		min-width: 180px;
	}
	.memory-center-scope {
		width: 150px;
		flex: 0 0 auto;
	}
	.memory-center-source {
		width: 125px;
		flex: 0 0 auto;
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
	}
</style>
