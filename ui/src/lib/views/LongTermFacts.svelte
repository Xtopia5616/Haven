<script>
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';

	let {
		facts = [],
		factsLoaded = false,
		factSourceFilter = '',
		factSourceOptions = [],
		newFact = { predicate: '', object: '', tags: '' },
		addingFact = false,
		onFactSourceFilterChange = () => {},
		onAddFact = () => {},
		onDeleteFact = () => {},
	} = $props();
	let selectedFactId = $state(null);
	const selectedFact = $derived.by(
		() => facts.find((fact) => fact.id === selectedFactId) || facts[0] || null,
	);
	$effect(() => {
		if (selectedFact && selectedFactId !== selectedFact.id) selectedFactId = selectedFact.id;
		if (!selectedFact) selectedFactId = null;
	});
	/** @param {string} value */
	function handleSourceChange(value) {
		onFactSourceFilterChange(value);
	}
</script>

<div class="facts-view">
	<div class="facts-toolbar workspace-filter-bar">
		<div class="section-heading">
			<h2>长期记忆</h2>
			<span class="section-count md-chip">{facts.length} 条</span>
		</div>
		<div class="facts-filter">
			<MaterialSelect
				value={factSourceFilter}
				ariaLabel="记忆来源"
				options={factSourceOptions}
				onChange={handleSourceChange}
			/>
		</div>
	</div>
	<p class="section-hint">
		跨会话长期记忆（身份、偏好、工作区等）。你可以手动添加/删除，Agent 也可以通过 memory
		工具更新。
	</p>

	<div class="facts-layout">
		<section class="fact-browser md-card md-card--outlined" aria-labelledby="fact-list-title">
			<div class="section-head">
				<div>
					<h3 id="fact-list-title">已保存的记忆</h3>
					<p>选择一条查看完整内容。</p>
				</div>
				<span class="section-count">{facts.length} 条</span>
			</div>
			{#if factsLoaded && facts.length > 0}
				<div class="fact-list" role="listbox" aria-label="长期记忆列表">
					{#each facts as fact (fact.id)}
						<button
							class="fact-row"
							class:selected={selectedFact?.id === fact.id}
							type="button"
							role="option"
							aria-selected={selectedFact?.id === fact.id}
							onclick={() => (selectedFactId = fact.id)}
						>
							<span class="fact-row-copy">
								<span class="fact-key">
									{#if fact.subject !== 'user'}{fact.subject}:{/if}{fact.predicate}
								</span>
								<span class="fact-object">{fact.object}</span>
							</span>
							<span
								class="fact-tag"
								class:fact-tag--inf={fact.source === 'inferred'}
								class:fact-tag--user={fact.source !== 'inferred'}
								>{fact.source === 'inferred' ? '推断' : '手动'}</span
							>
							<span class="fact-row-arrow" aria-hidden="true">→</span>
						</button>
					{/each}
				</div>
			{:else if factsLoaded}
				<div class="empty-inline">
					<span class="empty-inline-mark" aria-hidden="true">＋</span>
					<strong>还没有长期记忆</strong>
					<p>从右侧添加一条，或继续使用 Haven 让它自动抽取。</p>
				</div>
			{:else}
				<div class="empty-inline" role="status" aria-live="polite">
					<span class="empty-inline-mark" aria-hidden="true">…</span>
					<strong>正在加载长期记忆</strong>
				</div>
			{/if}
		</section>

		<aside class="fact-side">
			<section class="fact-editor md-card md-card--outlined" aria-labelledby="add-fact-title">
				<div class="section-head section-head--compact">
					<div>
						<h3 id="add-fact-title">添加一条记忆</h3>
						<p>用主语、谓词和对象描述一个事实。</p>
					</div>
				</div>
				<div class="fact-form">
					<label for="fact-predicate">谓词</label>
					<input
						id="fact-predicate"
						type="text"
						class="md-input"
						placeholder="例如：喜欢、email"
						bind:value={newFact.predicate}
						autocomplete="off"
					/>
					<label for="fact-object">对象</label>
					<input
						id="fact-object"
						type="text"
						class="md-input"
						placeholder="例如：深色主题"
						bind:value={newFact.object}
						autocomplete="off"
					/>
					<label for="fact-tags">标签 <span>可选</span></label>
					<input
						id="fact-tags"
						type="text"
						class="md-input"
						placeholder="逗号分隔"
						bind:value={newFact.tags}
						autocomplete="off"
					/>
				</div>
				<div class="fact-editor-actions">
					<MaterialButton
						variant="filled"
						label={addingFact ? '保存中…' : '保存记忆'}
						onclick={() => onAddFact()}
						disabled={addingFact}
					/>
				</div>
			</section>

			{#if selectedFact}
				{@const detailFact = selectedFact}
				<article
					class="fact-detail md-card md-card--outlined"
					aria-labelledby="fact-detail-title"
				>
					<div class="section-head section-head--compact">
						<div>
							<span class="fact-detail-kicker">当前选中</span>
							<h3 id="fact-detail-title">{detailFact.predicate}</h3>
						</div>
						<span
							class="fact-tag"
							class:fact-tag--inf={detailFact.source === 'inferred'}
							class:fact-tag--user={detailFact.source !== 'inferred'}
							>{detailFact.source === 'inferred' ? '推断' : '手动'}</span
						>
					</div>
					<dl class="fact-details">
						<div>
							<dt>主语</dt>
							<dd>{detailFact.subject || 'user'}</dd>
						</div>
						<div>
							<dt>谓词</dt>
							<dd>{detailFact.predicate}</dd>
						</div>
						<div>
							<dt>对象</dt>
							<dd>{detailFact.object}</dd>
						</div>
						{#if detailFact.tags}<div>
								<dt>标签</dt>
								<dd>
									{Array.isArray(detailFact.tags)
										? detailFact.tags.join('、')
										: detailFact.tags}
								</dd>
							</div>{/if}
						<div>
							<dt>编号</dt>
							<dd class="fact-id">{detailFact.id}</dd>
						</div>
					</dl>
					<MaterialButton
						variant="danger"
						label="删除这条事实"
						onclick={() => onDeleteFact(detailFact.id)}
					/>
				</article>
			{/if}
		</aside>
	</div>
</div>

<style>
	.facts-view {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.facts-toolbar {
		justify-content: space-between;
		margin-bottom: 0;
	}
	.section-heading,
	.section-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.section-heading h2,
	.section-head h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.section-head h3 {
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.section-head p,
	.section-hint {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.section-hint {
		margin: 0 0 var(--md-sys-space-sm);
	}
	.section-count {
		flex: 0 0 auto;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		white-space: nowrap;
	}
	.facts-filter {
		width: 140px;
		flex: 0 0 auto;
	}
	.facts-layout {
		display: grid;
		grid-template-columns: minmax(0, 1.15fr) minmax(280px, 0.85fr);
		align-items: start;
		gap: var(--md-sys-space-lg);
		min-width: 0;
	}
	.fact-browser,
	.fact-editor,
	.fact-detail {
		min-width: 0;
	}
	.fact-browser {
		min-height: 240px;
	}
	.fact-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		margin-top: var(--md-sys-space-lg);
	}
	.fact-row {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-width: 0;
		padding: var(--md-sys-space-md);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-medium);
		background: transparent;
		font: inherit;
		text-align: left;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.fact-row:hover,
	.fact-row.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
	}
	.fact-row:focus-visible {
		outline: none;
		box-shadow: var(--md-sys-focus-ring);
	}
	.fact-row-copy {
		display: grid;
		gap: var(--md-sys-space-2xs);
		min-width: 0;
		flex: 1;
	}
	.fact-key,
	.fact-object {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.fact-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.fact-object {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.fact-row-arrow {
		flex: 0 0 auto;
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: 1;
		transition: transform var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.fact-row:hover .fact-row-arrow,
	.fact-row.selected .fact-row-arrow {
		transform: translateX(var(--md-sys-space-xs));
	}
	.fact-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-height: var(--md-sys-space-xl);
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
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
	.fact-side {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
		min-width: 0;
	}
	.section-head--compact {
		align-items: flex-start;
	}
	.fact-form {
		display: grid;
		gap: var(--md-sys-space-xs);
		margin-top: var(--md-sys-space-lg);
	}
	.fact-form label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.fact-form label span {
		font-weight: 400;
	}
	.fact-editor-actions {
		display: flex;
		justify-content: flex-end;
		margin-top: var(--md-sys-space-lg);
	}
	.fact-detail {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
	}
	.fact-detail-kicker {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.fact-detail h3 {
		margin-top: var(--md-sys-space-xs);
	}
	.fact-details {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		margin: 0;
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
	.empty-inline {
		display: grid;
		justify-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-3xl) var(--md-sys-space-lg);
		text-align: center;
	}
	.empty-inline-mark {
		display: grid;
		place-items: center;
		width: var(--md-comp-button-touch-height);
		height: var(--md-comp-button-touch-height);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-size: var(--md-sys-typescale-headline-medium-size);
	}
	.empty-inline p {
		max-width: 320px;
		margin: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	@media (max-width: 800px) {
		.facts-layout {
			grid-template-columns: 1fr;
		}
	}
	@media (max-width: 640px) {
		.facts-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.facts-filter {
			width: 100%;
		}
	}
	@media (max-width: 455px) {
		.fact-row {
			align-items: flex-start;
			flex-wrap: wrap;
		}
		.fact-row-copy {
			min-width: calc(100% - 56px);
		}
		.fact-tag {
			margin-left: var(--md-sys-space-2xl);
		}
		.fact-details {
			grid-template-columns: 1fr;
		}
		.fact-editor-actions,
		.fact-editor-actions :global(.md-btn),
		.fact-detail :global(.md-btn) {
			width: 100%;
		}
	}
</style>
