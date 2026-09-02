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
	/** @param {string} value */
	function handleSourceChange(value) {
		onFactSourceFilterChange(value);
	}
</script>

<div class="section facts-view">
	<div class="toolbar">
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
			{#each facts as fact}<div class="fact-row">
					<span class="fact-key"
						>{#if fact.subject !== 'user'}{fact.subject}:{/if}{fact.predicate}</span
					><span class="fact-value"
						>{#if fact.source === 'inferred'}<span class="fact-tag fact-tag--inf"
								>推断</span
							>{:else}<span class="fact-tag fact-tag--user">手动</span
							>{/if}{fact.object}</span
					><button
						class="md-btn md-btn--xs md-btn--outlined"
						onclick={() => onDeleteFact(fact.id)}
						title="删除事实">&times;</button
					>
				</div>{/each}
		</div>
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
		gap: var(--md-sys-space-md);
		min-height: var(--md-comp-button-small-height);
	}
	.toolbar h2 {
		margin: 0;
		line-height: var(--md-comp-button-small-height);
		font-size: 18px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.toolbar-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-actions :global(.md-select-container) {
		width: 140px;
	}
	.model-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-sm));
		margin-bottom: var(--md-sys-space-md);
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
		padding: var(--md-sys-space-xs) 0;
	}
	.fact-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		font-weight: 500;
		min-width: 140px;
		flex-shrink: 0;
	}
	.fact-value {
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
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
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
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
</style>
