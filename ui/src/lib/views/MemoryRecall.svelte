<script>
	/** @typedef {{ entity_id: string; text: string; score?: number; kind?: string }} MemoryRecallResult */
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import LoadingState from '$lib/LoadingState.svelte';
	import CountChip from '$lib/CountChip.svelte';

	let {
		memoryRecall,
		onRecallKindChange = () => {},
		onRunRecall = () => {},
		showToolbar = true,
		showHeading = true,
	} = $props();
	/** @param {string} value */
	function handleKindChange(value) {
		onRecallKindChange(value);
	}
	let recallResults = $derived(/** @type {MemoryRecallResult[]} */ (memoryRecall.results));
	let factResults = $derived(recallResults.filter((result) => result.kind === 'fact'));
	let episodeResults = $derived(recallResults.filter((result) => result.kind === 'episode'));
</script>

<section class="recall-view" aria-label="记忆结果">
	{#if showToolbar}
		<div class="recall-toolbar workspace-filter-bar" role="search">
			<label class="recall-search" for="memory-recall-query">
				<span class="sr-only">检索记忆</span>
				<input
					id="memory-recall-query"
					type="search"
					class="md-input"
					bind:value={memoryRecall.query}
					placeholder="输入关键词，例如：深色主题"
					onkeydown={(event) => {
						if (event.key === 'Enter') onRunRecall();
					}}
					autocomplete="off"
				/>
			</label>
			<div class="recall-kind">
				<MaterialSelect
					id="memory-recall-kind"
					value={memoryRecall.kind}
					ariaLabel="检索类型"
					options={[
						{ value: 'all', label: '全部记忆' },
						{ value: 'fact', label: '事实' },
						{ value: 'episode', label: '过去的对话' },
					]}
					onChange={handleKindChange}
				/>
			</div>
			<MaterialButton
				variant="filled"
				label={memoryRecall.loading ? '检索中…' : '开始检索'}
				ariaBusy={memoryRecall.loading}
				onclick={() => onRunRecall()}
				disabled={memoryRecall.loading}
			/>
		</div>
	{/if}

	{#if showHeading}
		<div class="recall-heading">
			<div>
				<h2 id="memory-recall-title">记忆结果</h2>
				<p>优先使用语义检索；未配置 Embedding Model 时会自动回退到关键词匹配。</p>
			</div>
			{#if memoryRecall.searched && !memoryRecall.loading}
				<CountChip count={memoryRecall.results.length} label="条结果" />
			{/if}
		</div>
	{/if}

	{#if memoryRecall.loading}
		<div class="recall-loading" role="status" aria-live="polite">
			<LoadingState label="正在检索记忆" detail="正在查找最相关的内容" />
		</div>
	{:else if memoryRecall.results.length > 0}
		<section class="recall-results-panel md-card md-card--outlined" aria-live="polite">
			<div class="results-header">
				<div>
					<h3>匹配结果</h3>
					<p>
						{memoryRecall.kind === 'all'
							? '事实与过去的对话分别检索并展示。'
							: '按相关度从高到低排列。'}
					</p>
				</div>
			</div>
			{#if memoryRecall.kind === 'all'}
				<div class="recall-result-groups">
					{#if factResults.length > 0}
						<section class="recall-result-group" aria-labelledby="fact-results-title">
							<h4 id="fact-results-title">长期事实</h4>
							<ol class="recall-results">
								{#each factResults as result, index (result.entity_id + result.text)}
									<li class="recall-result">
										<span class="recall-rank" aria-label={`第${index + 1}条`}
											>{index + 1}</span
										>
										<div class="recall-result-copy">
											<span class="recall-result-type">长期事实</span>
											<p>{result.text}</p>
										</div>
										<span class="recall-score" title="相关度"
											>{(result.score ?? 0).toFixed(2)}</span
										>
									</li>
								{/each}
							</ol>
						</section>
					{/if}
					{#if episodeResults.length > 0}
						<section
							class="recall-result-group"
							aria-labelledby="episode-results-title"
						>
							<h4 id="episode-results-title">过去的对话</h4>
							<ol class="recall-results">
								{#each episodeResults as result, index (result.entity_id + result.text)}
									<li class="recall-result">
										<span class="recall-rank" aria-label={`第${index + 1}条`}
											>{index + 1}</span
										>
										<div class="recall-result-copy">
											<span class="recall-result-type">过去的对话</span>
											<p>{result.text}</p>
										</div>
										<span class="recall-score" title="相关度"
											>{(result.score ?? 0).toFixed(2)}</span
										>
									</li>
								{/each}
							</ol>
						</section>
					{/if}
				</div>
			{:else}
				<ol class="recall-results">
					{#each recallResults as result, index (result.entity_id + result.text)}
						<li class="recall-result">
							<span class="recall-rank" aria-label={`第${index + 1}条`}
								>{index + 1}</span
							>
							<div class="recall-result-copy">
								<span class="recall-result-type"
									>{result.kind === 'fact' ? '长期事实' : '过去的对话'}</span
								>
								<p>{result.text}</p>
							</div>
							<span class="recall-score" title="相关度"
								>{(result.score ?? 0).toFixed(2)}</span
							>
						</li>
					{/each}
				</ol>
			{/if}
		</section>
	{:else if memoryRecall.searched}
		<div class="recall-empty md-card md-card--outlined" role="status" aria-live="polite">
			<span class="recall-empty-mark" aria-hidden="true">⌕</span>
			<h3>没有找到匹配记忆</h3>
			<p>换一个描述，或切换“事实 / 情景”后再试一次。</p>
		</div>
	{:else}
		<div class="recall-empty md-card md-card--outlined">
			<span class="recall-empty-mark" aria-hidden="true">⌕</span>
			<h3>从一句话开始检索</h3>
			<p>输入你想找的偏好、事实或过去发生的事情。</p>
		</div>
	{/if}
</section>

<style>
	.recall-view {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xl);
		min-width: 0;
	}
	.recall-toolbar {
		margin-bottom: 0;
	}
	.recall-search {
		flex: 1 1 auto;
		min-width: 0;
	}
	.recall-kind {
		width: 120px;
		flex: 0 0 auto;
	}
	.recall-heading,
	.results-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.recall-heading h2,
	.results-header h3,
	.recall-empty h3 {
		margin: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.results-header h3,
	.recall-empty h3 {
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.recall-heading p,
	.results-header p,
	.recall-empty p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.recall-results-panel {
		min-width: 0;
	}
	.recall-result-groups {
		display: grid;
		gap: var(--md-sys-space-lg);
	}
	.recall-result-group h4 {
		margin: var(--md-sys-space-lg) 0 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-small-size);
		font-weight: 650;
	}
	.recall-result-group .recall-results {
		margin-top: var(--md-sys-space-sm);
	}
	.recall-results {
		list-style: none;
		margin: var(--md-sys-space-lg) 0 0;
		padding: 0;
	}
	.recall-result {
		display: grid;
		grid-template-columns: var(--md-comp-button-small-height) minmax(0, 1fr) auto;
		align-items: start;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-md) 0;
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.recall-rank {
		display: grid;
		place-items: center;
		width: var(--md-comp-button-small-height);
		height: var(--md-comp-button-small-height);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
	.recall-result-copy {
		min-width: 0;
	}
	.recall-result-type {
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.recall-result-copy p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.recall-score {
		padding-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		font-variant-numeric: tabular-nums;
	}
	.recall-empty {
		display: grid;
		justify-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-3xl) var(--md-sys-space-2xl);
		text-align: center;
	}
	.recall-empty-mark {
		display: grid;
		place-items: center;
		width: var(--md-comp-button-touch-height);
		height: var(--md-comp-button-touch-height);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-size: var(--md-sys-typescale-headline-medium-size);
	}
	.recall-empty p {
		max-width: 360px;
	}
	.recall-loading :global(.loading-state) {
		padding-block: var(--md-sys-space-2xl);
	}
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
	@media (max-width: 640px) {
		.recall-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.recall-search,
		.recall-kind,
		.recall-kind :global(.md-select-container),
		.recall-toolbar :global(.md-btn) {
			width: 100%;
		}
	}
	@media (max-width: 455px) {
		.recall-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		.recall-result {
			grid-template-columns: var(--md-comp-button-small-height) minmax(0, 1fr);
		}
		.recall-score {
			grid-column: 2;
			padding-top: 0;
		}
	}
</style>
