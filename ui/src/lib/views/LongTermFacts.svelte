<script>
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import CountChip from '$lib/CountChip.svelte';

	let {
		facts = [],
		factsLoaded = false,
		factSourceFilter = '',
		factSourceOptions = [],
		newFact = { predicate: '', object: '', tags: '' },
		addingFact = false,
		showHeader = true,
		onFactSourceFilterChange = () => {},
		onAddFact = () => {},
		onDeleteFact = () => {},
	} = $props();

	let selectedFactId = $state(null);
	let detailOpen = $state(false);
	const selectedFact = $derived(facts.find((fact) => fact.id === selectedFactId) || null);

	$effect(() => {
		if (selectedFactId && !selectedFact) {
			selectedFactId = null;
			detailOpen = false;
		}
	});

	/** @param {any} fact */
	function factSourceLabel(fact) {
		return fact.source === 'inferred' ? '推断' : '手动';
	}

	/** @param {any} fact */
	function factSourceTone(fact) {
		return fact.source === 'inferred' ? 'secondary' : 'primary';
	}

	/** @param {any} fact */
	function factSubjectLabel(fact) {
		return fact.subject && fact.subject !== 'user' ? `关于 ${fact.subject}` : '关于你';
	}

	/** @param {any} fact */
	function factTitle(fact) {
		return fact.predicate || '未命名记忆';
	}

	/** @param {any} fact */
	function factSentence(fact) {
		const subject = fact.subject && fact.subject !== 'user' ? fact.subject : '你';
		return `${subject} · ${fact.predicate || '未命名'} · ${fact.object || '暂无内容'}`;
	}

	/** @param {number | undefined} confidence */
	function confidenceLabel(confidence) {
		if (typeof confidence !== 'number' || Number.isNaN(confidence)) return '置信度未知';
		return `置信度 ${Math.round(Math.max(0, Math.min(1, confidence)) * 100)}%`;
	}

	/** @param {any} fact */
	function reinforcementLabel(fact) {
		const count = Number(fact.mention_count || 0);
		return count > 0 ? `已复核 ${count} 次` : '尚未复核';
	}

	/** @param {string | undefined | null} value */
	function formatDate(value) {
		if (!value) return '未知';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return value;
		return new Intl.DateTimeFormat('zh-CN', {
			dateStyle: 'medium',
			timeStyle: 'short',
		}).format(date);
	}

	/** @param {any} fact */
	function tagsLabel(fact) {
		return Array.isArray(fact.tags) && fact.tags.length > 0 ? fact.tags.join('、') : '无';
	}

	/** @param {any} fact */
	function selectFact(fact) {
		selectedFactId = fact.id;
		detailOpen = true;
	}

	function closeDetail() {
		detailOpen = false;
	}

	function deleteSelectedFact() {
		if (!selectedFact) return;
		const factId = selectedFact.id;
		detailOpen = false;
		selectedFactId = null;
		onDeleteFact?.(factId);
	}

	/** @param {string} value */
	function handleSourceChange(value) {
		onFactSourceFilterChange(value);
	}
</script>

<div class="facts-view">
	{#if showHeader}
		<div class="facts-toolbar workspace-filter-bar">
			<div class="section-heading">
				<h2>已保存事实</h2>
				<CountChip count={facts.length} label="条" />
			</div>
			<div class="facts-filter">
				<MaterialSelect
					value={factSourceFilter}
					ariaLabel="事实来源"
					options={factSourceOptions}
					onChange={handleSourceChange}
				/>
			</div>
		</div>
		<p class="section-hint">
			跨会话保存的结构化事实（身份、偏好、工作区等）。点击条目查看完整来源和记忆状态。
		</p>
	{/if}

	<div class="facts-layout">
		<section class="fact-browser" aria-label="已保存事实">
			{#if factsLoaded && facts.length > 0}
				<div class="fact-list" role="list" aria-label="已保存事实列表">
					{#each facts as fact (fact.id)}
						<article
							class="fact-card workspace-item-card motion-list-item"
							class:selected={selectedFactId === fact.id && detailOpen}
						>
							<button
								class="fact-card-main workspace-item-card-main"
								type="button"
								aria-label={`查看${factTitle(fact)}详情`}
								onclick={() => selectFact(fact)}
							>
								<span class="fact-card-header workspace-item-card-header">
									<span class="fact-card-type" data-tone={factSourceTone(fact)}>
										<span
											class="fact-card-indicator"
											data-tone={factSourceTone(fact)}
											aria-hidden="true"
										></span>
										长期事实
									</span>
									<MaterialBadge
										text={factSourceLabel(fact)}
										variant={factSourceTone(fact)}
									/>
								</span>
								<strong class="fact-card-title">{factTitle(fact)}</strong>
								<span class="fact-card-summary">{fact.object || '暂无内容'}</span>
								<span class="fact-card-meta workspace-item-card-meta">
									<span>{factSubjectLabel(fact)}</span>
									<span class="fact-card-meta-separator" aria-hidden="true"
										>·</span
									>
									<span>{confidenceLabel(fact.confidence)}</span>
								</span>
								<span class="fact-card-footer workspace-item-card-footer">
									<span class="fact-card-id workspace-item-card-id"
										>{reinforcementLabel(fact)}</span
									>
								</span>
							</button>
							<div class="fact-card-actions workspace-item-card-actions">
								<MaterialButton
									variant="text"
									label="查看详情"
									onclick={() => selectFact(fact)}
								/>
								<MaterialButton
									variant="text"
									className="fact-delete"
									label="删除"
									onclick={() => onDeleteFact?.(fact.id)}
								/>
							</div>
						</article>
					{/each}
				</div>
			{:else if factsLoaded}
				<div class="empty-inline">
					<span class="empty-inline-mark" aria-hidden="true">＋</span>
					<strong>还没有已保存的事实</strong>
					<p>从右侧添加一条，或继续使用 Haven 让它自动抽取。</p>
				</div>
			{:else}
				<div class="empty-inline" role="status" aria-live="polite">
					<span class="empty-inline-mark" aria-hidden="true">…</span>
					<strong>正在加载已保存事实</strong>
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
		</aside>
	</div>
</div>

<MaterialDialog
	open={detailOpen && selectedFact !== null}
	title={selectedFact ? factTitle(selectedFact) : '记忆详情'}
	dialogClass="fact-dialog"
	onClose={closeDetail}
>
	{#snippet children()}
		{#if selectedFact}
			<div class="fact-dialog-content">
				<div class="fact-dialog-overview">
					<div class="fact-dialog-type-row">
						<span class="fact-card-type" data-tone={factSourceTone(selectedFact)}>
							<span
								class="fact-card-indicator"
								data-tone={factSourceTone(selectedFact)}
								aria-hidden="true"
							></span>
							{factSubjectLabel(selectedFact)}
						</span>
						<MaterialBadge
							text={factSourceLabel(selectedFact)}
							variant={factSourceTone(selectedFact)}
						/>
					</div>
					<p class="fact-dialog-summary">{factSentence(selectedFact)}</p>
				</div>

				<dl class="fact-facts">
					<div>
						<dt>主语</dt>
						<dd>{selectedFact.subject || 'user'}</dd>
					</div>
					<div>
						<dt>谓词</dt>
						<dd>{selectedFact.predicate}</dd>
					</div>
					<div>
						<dt>对象</dt>
						<dd>{selectedFact.object}</dd>
					</div>
					<div>
						<dt>置信度</dt>
						<dd>{confidenceLabel(selectedFact.confidence)}</dd>
					</div>
					<div>
						<dt>创建时间</dt>
						<dd>{formatDate(selectedFact.created_at)}</dd>
					</div>
					<div>
						<dt>最近复核</dt>
						<dd>{formatDate(selectedFact.last_seen_at)}</dd>
					</div>
					<div>
						<dt>复核次数</dt>
						<dd>{selectedFact.mention_count || 0} 次</dd>
					</div>
					<div>
						<dt>标签</dt>
						<dd>{tagsLabel(selectedFact)}</dd>
					</div>
					<div>
						<dt>持久度</dt>
						<dd>
							{Math.round(
								Math.max(0, Math.min(1, selectedFact.durability ?? 1)) * 100,
							)}%
						</dd>
					</div>
					<div>
						<dt>记忆编号</dt>
						<dd><code class="fact-code">{selectedFact.id}</code></dd>
					</div>
				</dl>

				{#if selectedFact.source_ref?.snippet}
					<section class="fact-dialog-section">
						<h4>来源摘录</h4>
						<p class="fact-detail-copy">{selectedFact.source_ref.snippet}</p>
					</section>
				{/if}

				<div class="fact-actions">
					<MaterialButton
						variant="danger"
						label="删除这条记忆"
						onclick={deleteSelectedFact}
					/>
				</div>
			</div>
		{/if}
	{/snippet}
</MaterialDialog>

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
	.section-head,
	.fact-dialog-type-row {
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
	.fact-editor {
		min-width: 0;
	}
	.fact-list {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		min-width: 0;
		max-height: min(620px, calc(100vh - 280px));
		overflow-y: auto;
		scrollbar-gutter: stable;
		padding: var(--md-sys-space-xs);
	}
	.fact-card-type {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.fact-card-type[data-tone='primary'] {
		color: var(--md-sys-color-primary);
	}
	.fact-card-type[data-tone='secondary'] {
		color: var(--md-sys-color-secondary);
	}
	.fact-card-indicator {
		width: var(--md-sys-space-sm);
		height: var(--md-sys-space-sm);
		flex: 0 0 auto;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-outline);
	}
	.fact-card-indicator[data-tone='primary'] {
		background: var(--md-sys-color-primary);
	}
	.fact-card-indicator[data-tone='secondary'] {
		background: var(--md-sys-color-secondary);
	}
	.fact-card-header :global(.md-badge),
	.fact-dialog-type-row :global(.md-badge) {
		flex: 0 0 auto;
	}
	.fact-card-title {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		min-width: 0;
		overflow: hidden;
		overflow-wrap: anywhere;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.fact-card-summary {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		min-width: 0;
		overflow: hidden;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.fact-card-meta-separator {
		flex: 0 0 auto;
		color: var(--md-sys-color-outline);
	}
	.fact-card-actions :global(.fact-delete) {
		color: var(--md-sys-color-error);
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
	.fact-dialog-content {
		min-width: 0;
	}
	.fact-dialog-overview {
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.fact-dialog-summary {
		margin: var(--md-sys-space-md) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.fact-facts {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		margin: var(--md-sys-space-xl) 0;
	}
	.fact-facts div {
		display: grid;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		padding: var(--md-sys-space-sm) 0;
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.fact-facts dt,
	.fact-dialog-section h4 {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.fact-facts dd {
		min-width: 0;
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.fact-code {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		word-break: break-all;
	}
	.fact-dialog-section {
		margin-top: var(--md-sys-space-lg);
	}
	.fact-dialog-section h4 {
		margin: 0 0 var(--md-sys-space-sm);
	}
	.fact-detail-copy {
		margin: 0;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.fact-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-xl);
	}
	:global(.fact-dialog) {
		width: min(640px, calc(100vw - var(--md-sys-space-2xl)));
		max-height: calc(100vh - var(--md-sys-space-2xl));
		overflow: hidden;
	}
	:global(.fact-dialog .md-dialog-body) {
		overflow-y: auto;
	}
	@media (max-width: 800px) {
		.facts-layout {
			grid-template-columns: 1fr;
		}
		.fact-list {
			max-height: none;
			overflow: visible;
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
	@media (max-width: 540px) {
		.fact-list {
			grid-template-columns: 1fr;
		}
		.fact-facts {
			grid-template-columns: 1fr;
		}
		:global(.fact-dialog) {
			width: calc(100vw - var(--md-sys-space-lg));
			max-height: calc(100vh - var(--md-sys-space-lg));
		}
	}
	@media (max-width: 455px) {
		.fact-editor-actions,
		.fact-editor-actions :global(.md-btn),
		.fact-actions :global(.md-btn) {
			width: 100%;
		}
		.fact-actions {
			flex-direction: column;
			align-items: stretch;
		}
	}
</style>
