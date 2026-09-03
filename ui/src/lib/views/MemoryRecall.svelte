<script>
	import MaterialSelect from '$lib/MaterialSelect.svelte';

	let { memoryRecall, onRecallKindChange = () => {}, onRunRecall = () => {} } = $props();
	/** @param {string} value */
	function handleKindChange(value) {
		onRecallKindChange(value);
	}
</script>

<div class="section recall-view">
	<h2>记忆检索</h2>
	<p class="model-hint">
		检索已存储的事实边与情景条目。配置了 Embedding Model 时使用语义检索，否则回退到关键词匹配。
	</p>
	<input
		id="memory-recall-query"
		type="text"
		class="md-input"
		bind:value={memoryRecall.query}
		placeholder="检索内容，如：深色主题"
		onkeydown={(event) => {
			if (event.key === 'Enter') onRunRecall();
		}}
		autocomplete="off"
	/>
	<div class="recall-actions">
		<MaterialSelect
			id="memory-recall-kind"
			value={memoryRecall.kind}
			options={[
				{ value: 'fact', label: '事实' },
				{ value: 'episode', label: '情景' },
			]}
			onChange={handleKindChange}
		/><button
			class="md-btn md-btn--filled"
			onclick={() => onRunRecall()}
			disabled={memoryRecall.loading}>{memoryRecall.loading ? '检索中…' : '检索'}</button
		>
	</div>
	{#if memoryRecall.results.length > 0}<ul class="recall-results">
			{#each memoryRecall.results as result (result.entity_id + result.text)}<li>
					<span class="recall-score">{(result.score ?? 0).toFixed(2)}</span><span
						class="recall-text">{result.text}</span
					>
				</li>{/each}
		</ul>{:else if memoryRecall.searched && !memoryRecall.loading}<p class="model-hint">
			无匹配结果。
		</p>{/if}
</div>

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.section h2 {
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.recall-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
	}
	.recall-actions :global(.md-select-container) {
		width: 200px;
		flex-shrink: 0;
	}
	.recall-results {
		list-style: none;
		margin: var(--md-sys-space-sm) 0 var(--md-sys-space-md);
		padding: 0;
		max-height: 220px;
		overflow-y: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-radius-md);
	}
	.recall-results li {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.recall-results li:last-child {
		border-bottom: none;
	}
	.recall-score {
		font-variant-numeric: tabular-nums;
		color: var(--md-sys-color-primary);
		min-width: 42px;
	}
	.recall-text {
		color: var(--md-sys-color-on-surface);
		overflow-wrap: anywhere;
	}
	@media (max-width: 455px) {
		.recall-actions,
		.recall-actions :global(.md-select-container),
		.recall-actions .md-btn {
			width: 100%;
		}
	}
</style>
