<script>
	/**
	 * Unified task list/detail view. The route owns loading, event merging and
	 * IPC; this component only presents task lifecycle and emits user intent.
	 */
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { scheduleModeLabel, taskKindLabel, taskTitle } from '$lib/taskTerminology.ts';

	let {
		runningSessions = [],
		runningBackgroundActions = [],
		pendingScheduledActions = [],
		completedActions = [],
		actionStatusLabel = /** @type {(status: string) => string} */ ((status) => status || ''),
		sessionTitleFor = () => '',
		actionDuration = () => '',
		scheduledActionCountdown = () => '',
		formatHistoryTime = () => '',
		onOpenSession = () => {},
		onCancel = () => {},
		onDeleteHistory = () => {},
		onNewSession = () => {},
	} = $props();

	let selectedTaskId = $state(null);
	let query = $state('');
	let filter = $state('all');

	const taskRows = $derived.by(() => [
		...runningSessions.map((session) => ({
			id: session.id,
			kind: 'foreground',
			title: session.title || session.input || '当前会话',
			subtitle: session.status === 'paused' ? '已暂停，可继续' : taskKindLabel('foreground'),
			status: session.status,
			sessionId: session.id,
			value: session,
		})),
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
		...completedActions.map((action) => ({
			id: action.id,
			kind: action.kind || 'background',
			title: taskTitle(action),
			subtitle: action.kind === 'scheduled' ? '已执行' : actionStatusLabel(action.status),
			status: action.status || 'completed',
			sessionId: action.sessionId,
			value: action,
		})),
	]);

	const filteredRows = $derived.by(() => {
		const normalized = query.trim().toLocaleLowerCase();
		return taskRows.filter((row) => {
			if (filter !== 'all' && row.kind !== filter) return false;
			if (!normalized) return true;
			return `${row.title} ${row.subtitle} ${row.sessionId || ''} ${row.value?.command || ''}`
				.toLocaleLowerCase()
				.includes(normalized);
		});
	});

	const selectedRow = $derived(
		filteredRows.find((row) => row.id === selectedTaskId) || filteredRows[0] || null,
	);

	$effect(() => {
		if (selectedRow && selectedTaskId !== selectedRow.id) selectedTaskId = selectedRow.id;
		if (!selectedRow) selectedTaskId = null;
	});

	/** @param {any} row */
	function rowStatus(row) {
		if (row.kind === 'foreground') return row.status === 'running' ? '运行中' : row.subtitle;
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
	function selectRow(row) {
		selectedTaskId = row.id;
	}

	/** @param {string} value */
	function handleFilterChange(value) {
		filter = value;
	}
</script>

<section class="task-center" aria-labelledby="task-center-title">
	<header class="page-heading task-heading">
		<div>
			<h1 id="task-center-title">任务中心</h1>
			<p>统一查看会话、后台任务、定时任务和已完成记录。</p>
		</div>
		<MaterialButton variant="outlined" label="新建会话" onclick={() => onNewSession?.()} />
	</header>

	<div class="task-toolbar md-toolbar" role="search">
		<label class="task-search">
			<span class="sr-only">搜索任务</span>
			<input class="md-input" type="search" placeholder="搜索任务或会话" bind:value={query} />
		</label>
		<div class="task-filter">
			<MaterialSelect
				id="task-filter"
				value={filter}
				ariaLabel="任务类型"
				options={[
					{ value: 'all', label: '全部类型' },
					{ value: 'foreground', label: '会话' },
					{ value: 'background', label: '后台任务' },
					{ value: 'scheduled', label: '定时任务' },
				]}
				onChange={handleFilterChange}
			/>
		</div>
	</div>

	{#if taskRows.length === 0}
		<div class="task-empty md-card" data-state="empty">
			<span class="task-empty-icon" aria-hidden="true">✓</span>
			<h2>暂无任务</h2>
			<p>发起一段对话或安排定时任务后，进度和结果会显示在这里。</p>
			<MaterialButton variant="filled" label="开始新会话" onclick={() => onNewSession?.()} />
		</div>
	{:else if filteredRows.length === 0}
		<AsyncState
			title="没有匹配的任务"
			message="换一个关键词或清除筛选条件。"
			actionLabel="清除筛选"
			onAction={() => {
				query = '';
				filter = 'all';
			}}
		/>
	{:else}
		<div class="task-layout">
			<div class="task-list" aria-label="任务列表">
				{#each filteredRows as row (row.id)}
					<button
						class="task-row"
						class:selected={selectedRow?.id === row.id}
						type="button"
						onclick={() => selectRow(row)}
					>
						<span class="task-row-indicator" data-tone={rowTone(row)} aria-hidden="true"
						></span>
						<span class="task-row-main">
							<strong>{row.title}</strong>
							<span>{row.subtitle}</span>
						</span>
						<span class="task-row-status" data-tone={rowTone(row)}
							>{rowStatus(row)}</span
						>
					</button>
				{/each}
			</div>

			{#if selectedRow}
				{@const detail = selectedRow.value}
				<article class="task-detail md-card" aria-labelledby="task-detail-title">
					<div class="task-detail-heading">
						<div>
							<span class="task-kicker"
								>{selectedRow.kind === 'foreground'
									? taskKindLabel('foreground')
									: selectedRow.kind === 'background'
										? taskKindLabel('background')
										: taskKindLabel('scheduled')}</span
							>
							<h2 id="task-detail-title">{selectedRow.title}</h2>
						</div>
						<span class="md-badge" data-variant={rowTone(selectedRow)}
							>{rowStatus(selectedRow)}</span
						>
					</div>
					<dl class="task-facts">
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
							<dd>{selectedRow.id}</dd>
						</div>
						{#if selectedRow.kind === 'background'}<div>
								<dt>耗时</dt>
								<dd>{actionDuration(detail)}</dd>
							</div>{/if}
						{#if selectedRow.kind === 'background' && detail.command}<div>
								<dt>执行命令</dt>
								<dd><code class="task-command">{detail.command}</code></dd>
							</div>{/if}
						{#if selectedRow.kind === 'scheduled'}<div>
								<dt>执行时间</dt>
								<dd>{scheduledActionCountdown(detail.dueAt)}</dd>
							</div>{/if}
						{#if selectedRow.kind !== 'foreground' && detail.finishedAt}<div>
								<dt>完成时间</dt>
								<dd>{formatHistoryTime(detail)}</dd>
							</div>{/if}
					</dl>
					{#if detail.body}<p class="task-detail-copy">{detail.body}</p>{/if}
					{#if detail.output || detail.errorReason || detail.error}<pre
							class="task-output">{detail.output ||
								detail.errorReason ||
								detail.error}</pre>{/if}
					<div class="task-actions">
						{#if selectedRow.sessionId}
							<MaterialButton
								variant="outlined"
								label="打开来源会话"
								onclick={() => onOpenSession?.(selectedRow.sessionId)}
							/>
						{/if}
						{#if selectedRow.kind === 'background' && detail.status === 'running'}
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
						{#if selectedRow.kind !== 'foreground' && selectedRow.status !== 'running'}
							<MaterialButton
								variant="text"
								className="task-delete"
								label="删除记录"
								onclick={() => onDeleteHistory?.(selectedRow.id)}
							/>
						{/if}
					</div>
				</article>
			{/if}
		</div>
	{/if}
</section>

<style>
	.task-center {
		width: 100%;
		min-width: 0;
		min-height: 100%;
		container-type: inline-size;
	}
	.task-heading {
		align-items: center;
		flex-wrap: wrap;
	}
	.task-heading > div {
		min-width: 0;
	}
	.task-toolbar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-comp-toolbar-gap);
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
	.task-layout {
		display: grid;
		grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.4fr);
		gap: var(--md-sys-space-lg);
		align-items: start;
	}
	.task-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.task-row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--md-sys-space-md);
		width: 100%;
		min-width: 0;
		min-height: 64px;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		text-align: left;
		cursor: pointer;
	}
	.task-row:hover,
	.task-row.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
	}
	.task-row-indicator {
		width: 8px;
		height: 8px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-outline);
		flex: 0 0 auto;
	}
	.task-row-indicator[data-tone='running'] {
		background: var(--md-sys-color-success);
	}
	.task-row-indicator[data-tone='error'] {
		background: var(--md-sys-color-error);
	}
	.task-row-indicator[data-tone='scheduled'] {
		background: var(--md-sys-color-tertiary);
	}
	.task-row-indicator[data-tone='success'] {
		background: var(--md-sys-color-success);
	}
	.task-row-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		min-width: 0;
		flex: 1;
	}
	.task-row-main strong,
	.task-row-main span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.task-row-main strong {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow-wrap: anywhere;
	}
	.task-row-main span {
		white-space: nowrap;
	}
	.task-row-main strong {
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.task-row-main span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-row-status {
		min-width: 0;
		max-width: 40%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-row-status[data-tone='running'],
	.task-row-status[data-tone='success'] {
		color: var(--md-sys-color-success);
	}
	.task-row-status[data-tone='error'] {
		color: var(--md-sys-color-error);
	}
	.task-detail {
		min-width: 0;
	}
	.task-detail-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.task-detail-heading > div {
		min-width: 0;
		flex: 1 1 auto;
	}
	.task-kicker {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-detail h2 {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-headline-medium-size);
		line-height: var(--md-sys-typescale-headline-medium-line-height);
		overflow-wrap: anywhere;
	}
	.task-detail-heading .md-badge {
		min-width: 0;
		max-width: 36%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.task-facts {
		display: grid;
		gap: var(--md-sys-space-md);
		margin: var(--md-sys-space-xl) 0;
	}
	.task-facts div {
		display: grid;
		gap: var(--md-sys-space-xs);
	}
	.task-facts dt {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-facts dd {
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.task-detail-copy {
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.task-output {
		max-height: 240px;
		margin-top: var(--md-sys-space-lg);
		padding: var(--md-sys-space-md);
		overflow: auto;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font: var(--md-sys-typescale-code-size)/var(--md-sys-typescale-code-line-height)
			var(--md-sys-typescale-mono);
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
	:global(.task-delete) {
		color: var(--md-sys-color-error);
	}
	.task-empty {
		display: grid;
		justify-items: center;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-4xl) var(--md-sys-space-2xl);
		text-align: center;
	}
	.task-empty h2 {
		font-size: var(--md-sys-typescale-headline-medium-size);
		line-height: var(--md-sys-typescale-headline-medium-line-height);
	}
	.task-empty p {
		max-width: 420px;
		color: var(--md-sys-color-on-surface-variant);
		overflow-wrap: anywhere;
	}
	.task-empty-icon {
		display: grid;
		place-items: center;
		width: 48px;
		height: 48px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-size: 24px;
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
		.task-layout {
			grid-template-columns: 1fr;
		}
		.task-detail {
			order: -1;
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
		.task-actions {
			flex-direction: column;
			align-items: stretch;
		}
		.task-actions :global(.md-btn) {
			width: 100%;
		}
	}
	@container (max-width: 420px) {
		.task-row {
			padding-inline: var(--md-sys-space-sm);
		}
		.task-row-status {
			max-width: 34%;
		}
	}
	@container (max-width: 360px) {
		.task-row-status {
			display: none;
		}
	}
	@media (max-width: 455px) {
		.task-heading,
		.task-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.task-heading > div,
		.task-search,
		.task-filter {
			width: 100%;
		}
		.task-search {
			flex: 0 1 auto;
		}
		.task-heading :global(.md-btn) {
			width: 100%;
		}
	}
</style>
