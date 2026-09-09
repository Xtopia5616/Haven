<script>
	/**
	 * Unified task list/detail view. The route owns loading, event merging and
	 * IPC; this component only presents task lifecycle and emits user intent.
	 */
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import { scheduleModeLabel, taskKindLabel, taskTitle } from '$lib/taskTerminology.ts';

	let {
		runningBackgroundActions = [],
		pendingScheduledActions = [],
		actionStatusLabel = /** @type {(status: string) => string} */ ((status) => status || ''),
		sessionTitleFor = () => '',
		actionDuration = () => '',
		scheduledActionCountdown = () => '',
		onOpenSession = () => {},
		onCancel = () => {},
	} = $props();

	let selectedTaskId = $state(null);
	let detailOpen = $state(false);
	let query = $state('');
	let filter = $state('all');

	const taskRows = $derived.by(() => [
		...runningBackgroundActions.map((action) => ({
			id: action.id,
			kind: 'background',
			title: taskTitle(action),
			subtitle: sessionTitleFor(action) || '后台任务',
			status: action.status,
			sessionId: action.sessionId,
			value: action,
		})),
		...pendingScheduledActions.map((action) => ({
			id: action.id,
			kind: 'scheduled',
			title: taskTitle(action),
			subtitle: scheduleModeLabel(action.mode),
			status: 'scheduled',
			sessionId: action.sessionId,
			value: action,
		})),
	]);

	const filteredRows = $derived.by(() => {
		const normalized = query.trim().toLocaleLowerCase();
		return taskRows.filter((row) => {
			if (filter !== 'all' && row.kind !== filter) return false;
			if (!normalized) return true;
			return `${row.title} ${row.subtitle} ${row.sessionId || ''} ${row.value?.command || ''} ${row.value?.body || ''} ${row.value?.preview || ''}`
				.toLocaleLowerCase()
				.includes(normalized);
		});
	});
	const hasFilters = $derived(Boolean(query.trim() || filter !== 'all'));

	const selectedRow = $derived(taskRows.find((row) => row.id === selectedTaskId) || null);

	const taskGroups = $derived.by(() => {
		return [
			{
				id: 'actions',
				label: '进行中与待执行',
				description: '可取消的后台执行，以及尚未触发的定时任务。',
				rows: filteredRows,
			},
		].filter((group) => group.rows.length > 0);
	});

	$effect(() => {
		if (!selectedRow) {
			selectedTaskId = null;
			detailOpen = false;
		}
	});

	/** @param {any} row */
	function rowStatus(row) {
		if (row.kind === 'scheduled') return row.status === 'scheduled' ? '待执行' : '已执行';
		return actionStatusLabel(row.status);
	}

	/** @param {any} row */
	function rowTone(row) {
		if (row.kind === 'scheduled') return row.status === 'scheduled' ? 'scheduled' : 'success';
		if (row.status === 'failed') return 'error';
		if (row.status === 'completed') return 'success';
		return row.status === 'running' ? 'running' : 'neutral';
	}

	/** @param {any} row */
	function rowSummary(row) {
		const value = row.value || {};
		const candidates = [value.command, value.preview, value.body];
		for (const candidate of candidates) {
			if (
				typeof candidate === 'string' &&
				candidate.trim() &&
				candidate.trim().toLocaleLowerCase() !==
					String(row.title).trim().toLocaleLowerCase()
			) {
				return candidate.trim();
			}
		}
		if (row.kind === 'scheduled') {
			return `将在${rowTiming(row)}执行 · ${scheduleModeLabel(value.mode)}`;
		}
		return '正在执行后台任务';
	}

	/** @param {any} row */
	function rowContext(row) {
		if (row.kind === 'scheduled') return scheduleModeLabel(row.value?.mode);
		return sessionTitleFor(row.value) || '无关联会话';
	}

	/** @param {any} row */
	function rowTiming(row) {
		if (row.kind === 'background') return actionDuration(row.value) || '耗时未知';
		return scheduledActionCountdown(row.value?.dueAt) || '时间未设置';
	}

	/** @param {any} row */
	function selectRow(row) {
		selectedTaskId = row.id;
		detailOpen = true;
	}

	/** @param {any} row */
	function openRow(row) {
		if (row.sessionId) {
			onOpenSession?.(row.sessionId);
			return;
		}
		selectRow(row);
	}

	function closeDetail() {
		detailOpen = false;
	}

	/** @param {string} value */
	function handleFilterChange(value) {
		filter = value;
	}

	function clearFilters() {
		query = '';
		filter = 'all';
	}
</script>

<section class="task-center" aria-label="任务">
	<div class="task-toolbar workspace-filter-bar" role="search">
		<label class="task-search">
			<span class="sr-only">搜索任务</span>
			<input
				class="md-input"
				type="search"
				placeholder="搜索任务标题、来源或命令"
				bind:value={query}
			/>
		</label>
		<div class="task-filter">
			<MaterialSelect
				id="task-filter"
				value={filter}
				ariaLabel="任务类型"
				options={[
					{ value: 'all', label: '全部类型' },
					{ value: 'background', label: '后台任务' },
					{ value: 'scheduled', label: '定时任务' },
				]}
				onChange={handleFilterChange}
			/>
		</div>
		{#if hasFilters}
			<MaterialButton variant="text" label="清除筛选" onclick={clearFilters} />
		{/if}
		<CountChip count={filteredRows.length} label="项任务" className="task-count" live />
	</div>

	{#if taskRows.length === 0}
		<AsyncState title="暂无任务" message="安排后台或定时任务后，执行状态和结果会显示在这里。" />
	{:else if filteredRows.length === 0}
		<AsyncState
			title="没有匹配的任务"
			message="换一个关键词或清除筛选条件。"
			actionLabel="清除筛选"
			onAction={clearFilters}
		/>
	{:else}
		<div class="task-list-panel">
			<div class="task-groups" aria-label="按生命周期分组的任务列表">
				{#each taskGroups as group (group.id)}
					<section class="task-group" aria-labelledby={`task-group-${group.id}`}>
						<div class="task-group-heading">
							<div>
								<h3 id={`task-group-${group.id}`}>{group.label}</h3>
								<CountChip
									count={group.rows.length}
									label="项任务"
									className="task-group-count"
								/>
							</div>
							<p>{group.description}</p>
						</div>
						<div class="task-list">
							{#each group.rows as row (row.id)}
								<article
									class="task-card workspace-item-card motion-list-item"
									class:selected={selectedTaskId === row.id && detailOpen}
								>
									<button
										class="task-card-main workspace-item-card-main"
										type="button"
										aria-label={row.sessionId
											? `打开${row.title}对应会话`
											: `查看${row.title}详情`}
										onclick={() => openRow(row)}
									>
										<span class="task-card-header workspace-item-card-header">
											<span class="task-card-type" data-tone={rowTone(row)}>
												<span
													class="task-card-indicator"
													data-tone={rowTone(row)}
													aria-hidden="true"
												></span>
												{taskKindLabel(row.kind)}
											</span>
											<span class="md-badge" data-variant={rowTone(row)}
												>{rowStatus(row)}</span
											>
										</span>
										<strong class="task-card-title">{row.title}</strong>
										<span class="task-card-summary">{rowSummary(row)}</span>
										<span class="task-card-meta workspace-item-card-meta">
											<span>{rowContext(row)}</span>
											<span
												class="task-card-meta-separator"
												aria-hidden="true">·</span
											>
											<span>{rowTiming(row)}</span>
										</span>
										<span class="task-card-footer workspace-item-card-footer">
											<span class="task-card-id workspace-item-card-id"
												>{row.id}</span
											>
										</span>
									</button>
									<div class="task-card-actions workspace-item-card-actions">
										{#if row.kind === 'background' && row.value.status === 'running'}
											<MaterialButton
												variant="danger"
												label="停止后台任务"
												onclick={() => onCancel?.(row.id, 'background')}
											/>
										{:else if row.kind === 'scheduled' && row.status === 'scheduled'}
											<MaterialButton
												variant="outlined"
												label="取消此定时任务"
												onclick={() => onCancel?.(row.id, 'scheduled')}
											/>
										{/if}
									</div>
								</article>
							{/each}
						</div>
					</section>
				{/each}
			</div>
		</div>
	{/if}
</section>

<MaterialDialog
	open={detailOpen && selectedRow !== null}
	title={selectedRow?.title || '任务详情'}
	dialogClass="task-dialog"
	onClose={closeDetail}
>
	{#snippet children()}
		{#if selectedRow}
			<div class="task-dialog-content">
				<div class="task-dialog-overview">
					<div class="task-dialog-type-row">
						<span class="task-card-type" data-tone={rowTone(selectedRow)}>
							<span
								class="task-card-indicator"
								data-tone={rowTone(selectedRow)}
								aria-hidden="true"
							></span>
							{taskKindLabel(selectedRow.kind)}
						</span>
						<span class="md-badge" data-variant={rowTone(selectedRow)}
							>{rowStatus(selectedRow)}</span
						>
					</div>
					<p class="task-dialog-summary">{rowSummary(selectedRow)}</p>
				</div>

				<dl class="task-facts">
					<div>
						<dt>任务类型</dt>
						<dd>{taskKindLabel(selectedRow.kind)}</dd>
					</div>
					<div>
						<dt>来源会话</dt>
						<dd>
							{selectedRow.sessionId
								? sessionTitleFor({ sessionId: selectedRow.sessionId }) ||
									selectedRow.sessionId
								: '无关联会话'}
						</dd>
					</div>
					<div>
						<dt>任务编号</dt>
						<dd><code class="task-code">{selectedRow.id}</code></dd>
					</div>
					{#if selectedRow.kind === 'background'}
						<div>
							<dt>耗时</dt>
							<dd>{actionDuration(selectedRow.value) || '耗时未知'}</dd>
						</div>
					{/if}
					{#if selectedRow.kind === 'background' && selectedRow.value.command}
						<div>
							<dt>执行命令</dt>
							<dd><code class="task-command">{selectedRow.value.command}</code></dd>
						</div>
					{/if}
					{#if selectedRow.kind === 'scheduled'}
						<div>
							<dt>执行时间</dt>
							<dd>{rowTiming(selectedRow)}</dd>
						</div>
					{/if}
				</dl>

				{#if selectedRow.value.body}
					<section class="task-dialog-section">
						<h4>任务内容</h4>
						<p class="task-detail-copy">{selectedRow.value.body}</p>
					</section>
				{/if}
				<div class="task-actions">
					{#if selectedRow.kind === 'background' && selectedRow.value.status === 'running'}
						<MaterialButton
							variant="danger"
							label="停止任务"
							onclick={() => onCancel?.(selectedRow.id, 'background')}
						/>
					{/if}
					{#if selectedRow.kind === 'scheduled'}
						<MaterialButton
							variant="danger"
							label="取消定时任务"
							onclick={() => onCancel?.(selectedRow.id, 'scheduled')}
						/>
					{/if}
				</div>
			</div>
		{/if}
	{/snippet}
</MaterialDialog>

<style>
	.task-center {
		width: 100%;
		min-width: 0;
		min-height: 100%;
		container-type: inline-size;
	}
	.task-toolbar {
		margin-bottom: var(--md-sys-space-lg);
	}
	.task-search {
		flex: 1 1 280px;
		min-width: 0;
	}
	.task-filter {
		flex: 0 1 220px;
		min-width: 0;
	}
	.task-list-panel {
		min-width: 0;
		/* The workspace-level frame is supplied by WorkspaceSurface. Keep this
		 * region as content rhythm so task and history views share one outer
		 * surface instead of stacking two competing cards. */
	}
	.task-group-heading > div {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.task-group-heading h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	:global(.task-count),
	:global(.task-group-count) {
		flex: 0 0 auto;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.task-groups {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xl);
	}
	.task-group {
		min-width: 0;
	}
	.task-group + .task-group {
		padding-top: var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.task-group-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin: 0 var(--md-sys-space-xs) var(--md-sys-space-sm);
	}
	.task-group-heading p {
		margin: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		text-align: right;
	}
	.task-list {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		min-width: 0;
		max-height: min(620px, calc(100vh - 280px));
		overflow-y: auto;
		scrollbar-gutter: stable;
		padding: var(--md-sys-space-xs);
	}
	.task-card-main:focus-visible,
	.task-card-actions :global(.md-btn:focus-visible) {
		z-index: 2;
	}
	:global(.task-dialog-type-row) {
		display: flex;
		align-items: center;
		min-width: 0;
	}
	:global(.task-dialog-type-row) {
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
	}
	.task-card-type {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-card-type[data-tone='running'] {
		color: var(--md-sys-color-success);
	}
	.task-card-type[data-tone='error'] {
		color: var(--md-sys-color-error);
	}
	.task-card-type[data-tone='scheduled'] {
		color: var(--md-sys-color-tertiary);
	}
	.task-card-type[data-tone='success'] {
		color: var(--md-sys-color-success);
	}
	.task-card-indicator {
		width: 8px;
		height: 8px;
		flex: 0 0 auto;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-outline);
	}
	.task-card-indicator[data-tone='running'] {
		background: var(--md-sys-color-success);
	}
	.task-card-indicator[data-tone='error'] {
		background: var(--md-sys-color-error);
	}
	.task-card-indicator[data-tone='scheduled'] {
		background: var(--md-sys-color-tertiary);
	}
	.task-card-indicator[data-tone='success'] {
		background: var(--md-sys-color-success);
	}
	.task-card-header :global(.md-badge),
	:global(.task-dialog-type-row .md-badge) {
		flex: 0 0 auto;
	}
	.task-card-title {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		overflow-wrap: anywhere;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.task-card-summary {
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
	.task-card-meta-separator {
		flex: 0 0 auto;
		color: var(--md-sys-color-outline);
	}
	.task-dialog-content {
		min-width: 0;
	}
	.task-dialog-overview {
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.task-dialog-summary {
		margin: var(--md-sys-space-md) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.task-facts {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		margin: var(--md-sys-space-xl) 0;
	}
	.task-facts div {
		display: grid;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		padding: var(--md-sys-space-sm) 0;
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.task-facts dt,
	.task-dialog-section h4 {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-facts dd {
		min-width: 0;
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.task-code,
	.task-command {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		word-break: break-all;
	}
	.task-dialog-section {
		margin-top: var(--md-sys-space-lg);
	}
	.task-dialog-section h4 {
		margin: 0 0 var(--md-sys-space-sm);
	}
	.task-detail-copy {
		margin: 0;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.task-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-xl);
		min-width: 0;
	}
	.task-actions :global(.md-btn) {
		min-width: 0;
		max-width: 100%;
		white-space: normal;
		overflow-wrap: anywhere;
	}
	:global(.task-dialog) {
		width: min(640px, calc(100vw - var(--md-sys-space-2xl)));
		max-height: calc(100vh - var(--md-sys-space-2xl));
		overflow: hidden;
	}
	:global(.task-dialog .md-dialog-body) {
		overflow-y: auto;
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
	@container (max-width: 800px) {
		.task-list {
			grid-template-columns: 1fr;
			max-height: none;
			overflow: visible;
			padding-right: 0;
		}
	}
	@container (max-width: 520px) {
		.task-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.task-search,
		.task-filter {
			width: 100%;
			flex: 0 1 auto;
		}
		.task-group-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		.task-group-heading p {
			text-align: left;
		}
		.task-card {
			min-height: 0;
		}
		.task-card-main {
			padding: var(--md-sys-space-md);
		}
		.task-card-actions {
			padding-inline: var(--md-sys-space-md);
		}
		.task-actions {
			flex-direction: column;
			align-items: stretch;
		}
		.task-actions :global(.md-btn) {
			width: 100%;
		}
		.task-card-actions {
			justify-content: stretch;
		}
		.task-card-actions :global(.md-btn) {
			flex: 1;
		}
	}
	@media (max-width: 540px) {
		.task-facts {
			grid-template-columns: 1fr;
		}
		:global(.task-dialog) {
			width: calc(100vw - var(--md-sys-space-lg));
			max-height: calc(100vh - var(--md-sys-space-lg));
		}
	}
	@media (max-width: 455px) {
		.task-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.task-search,
		.task-filter {
			width: 100%;
		}
		.task-search {
			flex: 0 1 auto;
		}
		:global(.task-count) {
			align-self: flex-start;
		}
	}
</style>
