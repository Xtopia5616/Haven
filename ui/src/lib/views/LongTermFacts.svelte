<script>
	import MaterialSelect from '$lib/MaterialSelect.svelte';

	let {
		facts = [],
		factsLoaded = false,
		factSourceFilter = '',
		factSourceOptions = [],
		newFact,
		addingFact = false,
		onFactSourceFilterChange = () => {},
		onAddFact = () => {},
		onDeleteFact = () => {},
	} = $props();
	let selectedFactId = $state(null);
	const selectedFact = $derived.by(() => facts.find((fact) => fact.id === selectedFactId) || facts[0] || null);
	$effect(() => {
		if (selectedFact && selectedFactId !== selectedFact.id) selectedFactId = selectedFact.id;
		if (!selectedFact) selectedFactId = null;
	});
	/** @param {string} value */
	function handleSourceChange(value) {
		onFactSourceFilterChange(value);
	}
</script>

<div class="section facts-view">
	<div class="toolbar md-toolbar">
		<h2>长期事实</h2>
		<div class="toolbar-actions">
			<MaterialSelect
				value={factSourceFilter}
				options={factSourceOptions}
				onChange={handleSourceChange}
			/>
		</div>
	</div>
	<p class="model-hint">
		跨会话长期记忆（身份、偏好、工作区等）。可手动添加/删除；Agent 也可通过 memory 工具的
		remember / forget 更新。
	</p>
	<input
		type="text"
		class="md-input"
		placeholder="谓词（如 email）"
		bind:value={newFact.predicate}
		autocomplete="off"
	/>
	<input
		type="text"
		class="md-input"
		placeholder="对象（如 alice@example.com）"
		bind:value={newFact.object}
		autocomplete="off"
	/>
	<input
		type="text"
		class="md-input"
		placeholder="标签（可选，逗号分隔）"
		bind:value={newFact.tags}
		autocomplete="off"
	/>
	<div class="add-fact-actions">
		<button class="md-btn md-btn--filled" onclick={() => onAddFact()} disabled={addingFact}
			>{addingFact ? '添加中…' : '添加事实'}</button
		>
	</div>
	{#if factsLoaded && facts.length > 0}
		<div class="fact-list">
			{#each facts as fact (fact.id)}
				<button
					class="fact-row"
					class:selected={selectedFact?.id === fact.id}
					type="button"
					onclick={() => (selectedFactId = fact.id)}
				>
					<span class="fact-key">
						{#if fact.subject !== 'user'}{fact.subject}:{/if}{fact.predicate}
					</span>
					<span class="fact-value">
						{#if fact.source === 'inferred'}
							<span class="fact-tag fact-tag--inf">推断</span>
						{:else}
							<span class="fact-tag fact-tag--user">手动</span>
						{/if}
						{fact.object}
					</span>
				</button>
			{/each}
		</div>
		{#if selectedFact}
			{@const detailFact = selectedFact}
			<article class="fact-detail md-card" aria-labelledby="fact-detail-title">
				<div class="fact-detail-head">
					<div>
						<span class="fact-detail-kicker">记忆条目详情</span>
						<h3 id="fact-detail-title">{detailFact.predicate}</h3>
					</div>
					<span class="fact-tag" class:fact-tag--inf={detailFact.source === 'inferred'} class:fact-tag--user={detailFact.source !== 'inferred'}>{detailFact.source === 'inferred' ? '推断' : '手动'}</span>
				</div>
				<dl class="fact-details">
					<div><dt>主语</dt><dd>{detailFact.subject || 'user'}</dd></div>
					<div><dt>谓词</dt><dd>{detailFact.predicate}</dd></div>
					<div><dt>对象</dt><dd>{detailFact.object}</dd></div>
					{#if detailFact.tags}<div><dt>标签</dt><dd>{Array.isArray(detailFact.tags) ? detailFact.tags.join('、') : detailFact.tags}</dd></div>{/if}
					<div><dt>编号</dt><dd class="fact-id">{detailFact.id}</dd></div>
				</dl>
				<button class="md-btn md-btn--danger" type="button" onclick={() => onDeleteFact(detailFact.id)}>删除这条事实</button>
			</article>
		{/if}
	{:else if factsLoaded}<p class="model-hint">
			暂无事实。使用 Haven 后会自动抽取并显示在这里。
		</p>{/if}
</div>

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-comp-toolbar-gap);
		min-height: var(--md-comp-button-small-height);
	}
	.toolbar h2 {
		margin: 0;
		line-height: var(--md-sys-typescale-title-large-line-height);
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.toolbar-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.toolbar-actions :global(.md-select-container) {
		width: 140px;
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.add-fact-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.fact-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.fact-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.fact-row:hover,
	.fact-row.selected {
		border-color: var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container);
	}
	.fact-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 500;
		line-height: var(--md-sys-typescale-body-small-line-height);
		min-width: 140px;
		flex-shrink: 0;
	}
	.fact-value {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.fact-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 20px;
		padding: 0 6px;
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		flex-shrink: 0;
	}
	.fact-tag--user {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.fact-tag--inf {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.fact-detail {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
		margin-top: var(--md-sys-space-lg);
	}
	.fact-detail-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.fact-detail-kicker {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.fact-detail h3 {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.fact-details {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
	}
	.fact-details div {
		display: grid;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.fact-details dt {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.fact-details dd {
		margin: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.fact-id {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size) !important;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	@media (max-width: 455px) {
		.toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.toolbar-actions,
		.toolbar-actions :global(.md-select-container),
		.add-fact-actions,
		.add-fact-actions .md-btn {
			width: 100%;
		}
		.fact-details { grid-template-columns: 1fr; }
		.fact-row { align-items: flex-start; flex-direction: column; }
		.fact-value { width: 100%; }
	}
</style>
