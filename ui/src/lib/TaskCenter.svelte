<script>
	/**
	 * Unified task list/detail view. The route owns loading, event merging and
	 * IPC; this component only presents task lifecycle and emits user intent.
	 */
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import { scheduleModeLabel, taskKindLabel, taskTitle } from '$lib/taskTerminology.ts';
	import WorkspaceMetricStrip from '$lib/WorkspaceMetricStrip.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceScopeNote from '$lib/WorkspaceScopeNote.svelte';

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
	let detailOpen = $state(false);
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
			return `${row.title} ${row.subtitle} ${row.sessionId || ''} ${row.value?.command || ''} ${row.value?.body || ''} ${row.value?.preview || ''}`
				.toLocaleLowerCase()
				.includes(normalized);
		});
	});
	const hasFilters = $derived(Boolean(query.trim() || filter !== 'all'));
	const metricItems = $derived([
		{
			id: 'current',
			value: runningSessions.length,
			label: '当前会话',
			detail: '正在处理或等待继续',
			tone: 'running',
		},
		{
			id: 'background',
			value: runningBackgroundActions.length,
			label: '处理中',
			detail: '后台任务',
			tone: 'running',
		},
		{
			id: 'scheduled',
			value: pendingScheduledActions.length,
			label: '待执行',
			detail: '定时任务',
			tone: 'scheduled',
		},
		{
			id: 'history',
			value: completedActions.length,
			label: '执行记录',
			detail: '已完成、失败或取消',
		},
	]);

	const selectedRow = $derived(taskRows.find((row) => row.id === selectedTaskId) || null);

	/** @param {any} row */
	function isLiveAction(row) {
		return row.status === 'running' || row.status === 'scheduled';
	}

	const taskGroups = $derived.by(() => {
		return [
			{
				id: 'session',
				label: '当前会话',
				description: '正在运行、排队或等待继续的会话。',
				rows: filteredRows.filter((row) => row.kind === 'foreground'),
			},
			{
				id: 'actions',
				label: '后台与定时任务',
				description: '可取消的后台执行，以及尚未触发的定时任务。',
				rows: filteredRows.filter((row) => row.kind !== 'foreground' && isLiveAction(row)),
			},
			{
				id: 'history',
				label: '执行记录',
				description: '已完成、失败或已取消的任务结果。',
				rows: filteredRows.filter((row) => row.kind !== 'foreground' && !isLiveAction(row)),
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
		if (row.kind === 'foreground') {
			if (row.status === 'running') return '运行中';
			if (row.status === 'paused' || String(row.status || '').startsWith('paused_'))
				return '已暂停';
			return actionStatusLabel(row.status) || row.subtitle;
		}
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
		const candidates =
			row.kind === 'foreground' ? [value.input] : [value.command, value.preview, value.body];
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
		if (row.kind === 'foreground') return '正在处理这段会话';
		if (row.kind === 'scheduled') {
			return `将在${rowTiming(row)}执行 · ${scheduleModeLabel(value.mode)}`;
		}
		if (row.status === 'failed') return '任务执行失败，打开详情查看原因';
		if (row.status === 'completed') return '任务已完成，打开详情查看执行结果';
		return '正在执行后台任务';
	}

	/** @param {any} row */
	function rowContext(row) {
		if (row.kind === 'foreground') return row.status === 'paused' ? '等待继续' : '当前会话';
		if (row.kind === 'scheduled') return scheduleModeLabel(row.value?.mode);
		return sessionTitleFor(row.value) || '无关联会话';
	}

	/** @param {any} row */
	function rowTiming(row) {
		if (row.kind === 'foreground') return row.status === 'paused' ? '可继续' : '正在处理';
		if (row.kind === 'background') return actionDuration(row.value) || '耗时未知';
		if (row.status === 'scheduled') {
			return scheduledActionCountdown(row.value?.dueAt) || '时间未设置';
		}
		return formatHistoryTime(row.value) || '已执行';
	}

	/** @param {any} row */
	function selectRow(row) {
		selectedTaskId = row.id;
		detailOpen = true;
	}

	function closeDetail() {
		detailOpen = false;
	}

	function openSelectedSession() {
		if (!selectedRow?.sessionId) return;
		detailOpen = false;
		onOpenSession?.(selectedRow.sessionId);
	}

	function deleteSelectedHistory() {
		if (!selectedRow) return;
		const id = selectedRow.id;
		detailOpen = false;
		onDeleteHistory?.(id);
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

<section class="task-center" aria-labelledby="task-center-title">
	<WorkspacePageHeader
		title="任务"
		description="查看正在执行、待执行和已完成的工作。"
		headingId="task-center-title"
	>
		{#snippet children()}
			<MaterialButton variant="filled" label="新建会话" onclick={() => onNewSession?.()} />
		{/snippet}
	</WorkspacePageHeader>

	<WorkspaceScopeNote
		title="任务负责执行与进度"
		message="在这里查看状态、取消任务和追踪结果；完整的会话历史与长期记忆请到“记忆”。"
	/>

	<WorkspaceMetricStrip items={metricItems} />

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
					{ value: 'foreground', label: '当前会话' },
					{ value: 'background', label: '后台任务' },
					{ value: 'scheduled', label: '定时任务' },
				]}
				onChange={handleFilterChange}
			/>
		</div>
		{#if hasFilters}
			<MaterialButton variant="text" label="清除筛选" onclick={clearFilters} />
		{/if}
	</div>

	{#if taskRows.length === 0}
		<AsyncState
			title="暂无任务"
			message="开始对话或安排后台、定时任务后，执行状态和结果会显示在这里。"
			actionLabel="开始新会话"
			onAction={() => onNewSession?.()}
		/>
	{:else if filteredRows.length === 0}
		<AsyncState
			title="没有匹配的任务"
			message="换一个关键词或清除筛选条件。"
			actionLabel="清除筛选"
			onAction={clearFilters}
		/>
	{:else}
		<div class="task-list-panel">
			<div class="task-list-heading">
				<div>
					<h2>任务列表</h2>
					<span class="task-list-count">{filteredRows.length} 项</span>
				</div>
				<span class="task-list-hint">查看详情，或直接执行右侧操作</span>
			</div>
			<div class="task-groups" aria-label="按生命周期分组的任务列表">
				{#each taskGroups as group (group.id)}
					<section class="task-group" aria-labelledby={`task-group-${group.id}`}>
						<div class="task-group-heading">
							<div>
								<h3 id={`task-group-${group.id}`}>{group.label}</h3>
								<span class="task-list-count">{group.rows.length} 项</span>
							</div>
							<p>{group.description}</p>
						</div>
						<div class="task-list">
							{#each group.rows as row (row.id)}
								<article
									class="task-card motion-list-item"
									class:selected={selectedTaskId === row.id && detailOpen}
								>
									<button
										class="task-card-main"
										type="button"
										aria-label={`查看${row.title}详情`}
										onclick={() => selectRow(row)}
									>
										<span class="task-card-header">
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
										<span class="task-card-meta">
											<span>{rowContext(row)}</span>
											<span
												class="task-card-meta-separator"
												aria-hidden="true">·</span
											>
											<span>{rowTiming(row)}</span>
										</span>
										<span class="task-card-footer">
											<span class="task-card-id">{row.id}</span>
											<span class="task-card-open" aria-hidden="true"
												>查看详情 <span>→</span></span
											>
										</span>
									</button>
									<div class="task-card-actions">
										{#if row.kind === 'foreground' && row.sessionId}
											<MaterialButton
												variant={row.status === 'paused'
													? 'filled'
													: 'tonal'}
												label={row.status === 'paused'
													? '继续会话'
													: '打开会话'}
												onclick={() => onOpenSession?.(row.sessionId)}
											/>
										{:else if row.kind === 'background' && row.value.status === 'running'}
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
										{:else}
											<MaterialButton
												variant="text"
												label="查看详情"
												onclick={() => selectRow(row)}
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
						<dt>{selectedRow.kind === 'foreground' ? '会话编号' : '来源会话'}</dt>
						<dd>
							{selectedRow.kind === 'foreground'
								? selectedRow.sessionId
								: selectedRow.sessionId
									? sessionTitleFor({ sessionId: selectedRow.sessionId }) ||
										selectedRow.sessionId
									: '无关联会话'}
						</dd>
					</div>
					<div>
						<dt>{selectedRow.kind === 'foreground' ? '运行编号' : '任务编号'}</dt>
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
					{#if selectedRow.kind !== 'foreground' && selectedRow.value.finishedAt}
						<div>
							<dt>完成时间</dt>
							<dd>{formatHistoryTime(selectedRow.value)}</dd>
						</div>
					{/if}
				</dl>

				{#if selectedRow.value.body}
					<section class="task-dialog-section">
						<h4>任务内容</h4>
						<p class="task-detail-copy">{selectedRow.value.body}</p>
					</section>
				{/if}
				{#if selectedRow.value.output || selectedRow.value.errorReason || selectedRow.value.error}
					<section class="task-dialog-section">
						<h4>
							{selectedRow.value.errorReason || selectedRow.value.error
								? '失败原因'
								: '执行结果'}
						</h4>
						<pre class="task-output">{selectedRow.value.output ||
								selectedRow.value.errorReason ||
								selectedRow.value.error}</pre>
					</section>
				{/if}

				<div class="task-actions">
					{#if selectedRow.sessionId}
						<MaterialButton
							variant="filled"
							label={selectedRow.kind === 'foreground' ? '打开会话' : '打开来源会话'}
							onclick={openSelectedSession}
						/>
					{/if}
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
					{#if selectedRow.kind !== 'foreground' && selectedRow.status !== 'running'}
						<MaterialButton
							variant="text"
							className="task-delete"
							label="删除记录"
							onclick={deleteSelectedHistory}
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
	.task-list-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		padding: 0 var(--md-sys-space-xs) var(--md-sys-space-md);
	}
	.task-list-heading > div,
	.task-group-heading > div {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-sm);
		min-width: 0;
	}
	.task-list-heading h2,
	.task-group-heading h3 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.task-list-count,
	.task-list-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-list-hint {
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
	.task-card {
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: stretch;
		width: 100%;
		min-width: 0;
		min-height: 168px;
		overflow: hidden;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		text-align: left;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.task-card-main {
		position: relative;
		display: flex;
		flex: 1;
		flex-direction: column;
		align-items: stretch;
		gap: var(--md-sys-space-sm);
		width: 100%;
		min-width: 0;
		min-height: 0;
		padding: var(--md-sys-space-lg);
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.task-card::after {
		position: absolute;
		inset: 0;
		content: '';
		background: currentColor;
		opacity: 0;
		pointer-events: none;
		transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.task-card > * {
		position: relative;
		z-index: 1;
	}
	.task-card:hover,
	.task-card.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
		box-shadow: var(--md-sys-elevation-2);
		transform: translateY(-1px);
	}
	.task-card:hover::after {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.task-card-main:focus-visible {
		outline: none;
		box-shadow: var(--md-sys-focus-ring), var(--md-sys-elevation-1);
	}
	.task-card-main:focus-visible,
	.task-card-actions :global(.md-btn:focus-visible) {
		z-index: 2;
	}
	.task-card-header,
	:global(.task-dialog-type-row),
	.task-card-footer,
	.task-card-meta {
		display: flex;
		align-items: center;
		min-width: 0;
	}
	.task-card-header,
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
	.task-card-meta {
		gap: var(--md-sys-space-xs);
		margin-top: auto;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.task-card-meta span:first-child {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.task-card-meta-separator {
		flex: 0 0 auto;
		color: var(--md-sys-color-outline);
	}
	.task-card-meta span:last-child {
		flex: 0 0 auto;
	}
	.task-card-footer {
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		padding-top: var(--md-sys-space-sm);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.task-card-id {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--md-sys-typescale-mono);
	}
	.task-card-open {
		flex: 0 0 auto;
		color: var(--md-sys-color-primary);
		font-weight: 650;
		white-space: nowrap;
	}
	.task-card-open span {
		display: inline-block;
		margin-left: var(--md-sys-space-2xs);
		transition: transform var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.task-card:hover .task-card-open span {
		transform: translateX(var(--md-sys-space-2xs));
	}
	.task-card-actions {
		position: relative;
		z-index: 1;
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		background: color-mix(
			in srgb,
			var(--md-sys-color-surface-container-low) 76%,
			var(--md-sys-color-surface) 24%
		);
	}
	.task-card-actions :global(.md-btn) {
		min-width: 0;
		white-space: nowrap;
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
	.task-output {
		max-height: 240px;
		margin: 0;
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
		.task-list-heading {
			align-items: flex-start;
			flex-direction: column;
			padding-inline: var(--md-sys-space-xs);
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
	}
</style>
