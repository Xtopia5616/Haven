<script lang="ts">
	/**
	 * Unified task list/detail view. The route owns loading, event merging and
	 * IPC; this component only presents task lifecycle and emits user intent.
	 */
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import HistoryListGroup from '$lib/HistoryListGroup.svelte';
	import { projectToolRunCard } from '$lib/toolRunCardProjection.ts';
	import { toolRunKindLabel } from '$lib/toolRunTerminology.ts';
	import type { ToolRunKind, ToolRunPayload, ToolRunStatus } from '$lib/contracts/toolRun.ts';
	import type {
		ToolRunCardProjection,
		ToolRunCardProjectionOptions,
	} from '$lib/toolRunCardProjection.ts';

	interface Props extends Partial<ToolRunCardProjectionOptions> {
		runningBackgroundToolRuns?: ToolRunPayload[];
		pendingScheduledToolRuns?: ToolRunPayload[];
		toolRunHistory?: ToolRunPayload[];
		toolRunHistoryLoading?: boolean;
		toolRunHistoryFailed?: boolean;
		onOpenSession?: (sessionId: string) => void;
		onCancel?: (toolRunId: string, kind?: ToolRunKind) => void;
		onRefreshTaskHistory?: () => void;
	}

	let {
		runningBackgroundToolRuns = [],
		pendingScheduledToolRuns = [],
		toolRunHistory = [],
		toolRunHistoryLoading = false,
		toolRunHistoryFailed = false,
		toolRunStatusLabel = (status) => status || '',
		sessionTitleFor = () => '',
		toolRunDuration = () => '',
		scheduledToolRunCountdown = () => '',
		onOpenSession = () => {},
		onCancel = () => {},
		onRefreshTaskHistory = () => {},
	}: Props = $props();

	let selectedToolRunId = $state<string | null>(null);
	let detailOpen = $state(false);
	let query = $state('');
	let filter = $state('all');

	const toolRunRows = $derived.by(() => {
		const options: ToolRunCardProjectionOptions = {
			toolRunStatusLabel,
			sessionTitleFor,
			toolRunDuration,
			scheduledToolRunCountdown,
		};
		const seenIds = new Set<string>();
		const toolRuns = [
			...runningBackgroundToolRuns.map((toolRun) => projectToolRunCard(toolRun, options)),
			...pendingScheduledToolRuns.map((toolRun) => projectToolRunCard(toolRun, options)),
			...toolRunHistory.map((toolRun) => projectToolRunCard(toolRun, options)),
		];
		return toolRuns.filter((row) => {
			if (seenIds.has(row.toolRunId)) return false;
			seenIds.add(row.toolRunId);
			return true;
		});
	});

	const filteredRows = $derived.by(() => {
		const normalized = query.trim().toLocaleLowerCase();
		return toolRunRows.filter((row) => {
			if (filter !== 'all' && row.kind !== filter) return false;
			if (!normalized) return true;
			return `${row.title} ${row.searchText} ${row.sessionId || ''} ${row.details.command || ''} ${row.details.body || ''} ${row.details.preview || ''} ${row.details.output || ''} ${row.details.error || ''}`
				.toLocaleLowerCase()
				.includes(normalized);
		});
	});
	const hasFilters = $derived(Boolean(query.trim() || filter !== 'all'));

	const selectedRow = $derived(
		toolRunRows.find((row) => row.toolRunId === selectedToolRunId) || null,
	);

	const toolRunGroups = $derived.by(() => {
		return [
			{
				id: 'tool-runs',
				label: '进行中',
				rows: filteredRows.filter(
					(row) => !['completed', 'failed', 'cancelled'].includes(row.status || ''),
				),
			},
			{
				id: 'history',
				label: '已结束',
				rows: filteredRows.filter((row) =>
					['completed', 'failed', 'cancelled'].includes(row.status || ''),
				),
			},
		].filter((group) => group.rows.length > 0);
	});

	$effect(() => {
		if (!selectedRow) {
			selectedToolRunId = null;
			detailOpen = false;
		}
	});

	function selectRow(row: ToolRunCardProjection) {
		selectedToolRunId = row.toolRunId;
		detailOpen = true;
	}

	function openRow(row: ToolRunCardProjection) {
		if (row.sessionId && !['completed', 'failed', 'cancelled'].includes(row.status || '')) {
			onOpenSession?.(row.sessionId);
			return;
		}
		selectRow(row);
	}

	function closeDetail() {
		detailOpen = false;
	}

	function handleFilterChange(value: string) {
		filter = value;
	}

	function clearFilters() {
		query = '';
		filter = 'all';
	}
</script>

<section class="task-center" aria-label="任务历史">
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

	{#if toolRunRows.length === 0 && toolRunHistoryLoading}
		<AsyncState
			state="loading"
			title="正在加载任务历史"
			message="正在读取已结束的后台任务和定时任务。"
		/>
	{:else if toolRunRows.length === 0 && toolRunHistoryFailed}
		<AsyncState
			state="error"
			title="任务历史加载失败"
			message="检查应用连接后重试。"
			actionLabel="重试"
			onAction={onRefreshTaskHistory}
		/>
	{:else if toolRunRows.length === 0}
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
				{#each toolRunGroups as group (group.id)}
					<HistoryListGroup
						id={`tasks-${group.id}`}
						title={group.label}
						count={group.rows.length}
					>
						{#snippet children()}
							{#each group.rows as row (row.toolRunId)}
								<article
									class="task-card workspace-item-card motion-list-item"
									class:selected={selectedToolRunId === row.toolRunId &&
										detailOpen}
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
											<span
												class="task-card-type workspace-item-card-kind"
												data-tone={row.tone}
											>
												<span
													class="workspace-item-card-indicator"
													data-tone={row.tone}
													aria-hidden="true"
												></span>
												{toolRunKindLabel(row.kind)}
											</span>
											<span class="md-badge" data-variant={row.tone}
												>{row.statusLabel}</span
											>
										</span>
										<strong class="workspace-item-card-title"
											>{row.title}</strong
										>
										<span class="workspace-item-card-summary"
											>{row.summary}</span
										>
										<span class="task-card-meta workspace-item-card-meta">
											<span>{row.context}</span>
											<span
												class="task-card-meta-separator"
												aria-hidden="true">·</span
											>
											<span>{row.timing}</span>
										</span>
										<span class="task-card-footer workspace-item-card-footer">
											<span class="workspace-item-card-id"
												>{row.toolRunId}</span
											>
											<span
												class="workspace-item-card-open"
												aria-hidden="true">打开</span
											>
										</span>
									</button>
									{#if (row.kind === 'background' && row.status === 'running') || (row.kind === 'scheduled' && (row.status === 'waiting' || row.status === 'running'))}
										<div class="task-card-actions workspace-item-card-actions">
											{#if row.kind === 'background' && row.status === 'running'}
												<MaterialButton
													variant="danger"
													label="停止后台任务"
													onclick={() =>
														onCancel?.(row.toolRunId, 'background')}
												/>
											{:else if row.kind === 'scheduled' && (row.status === 'waiting' || row.status === 'running')}
												<MaterialButton
													variant="outlined"
													label="取消此定时任务"
													onclick={() =>
														onCancel?.(row.toolRunId, 'scheduled')}
												/>
											{/if}
										</div>
									{/if}
								</article>
							{/each}
						{/snippet}
					</HistoryListGroup>
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
						<span
							class="task-card-type workspace-item-card-kind"
							data-tone={selectedRow.tone}
						>
							<span
								class="workspace-item-card-indicator"
								data-tone={selectedRow.tone}
								aria-hidden="true"
							></span>
							{toolRunKindLabel(selectedRow.kind)}
						</span>
						<span class="md-badge" data-variant={selectedRow.tone}
							>{selectedRow.statusLabel}</span
						>
					</div>
					<p class="task-dialog-summary">{selectedRow.summary}</p>
				</div>

				<dl class="task-facts">
					<div>
						<dt>任务类型</dt>
						<dd>{toolRunKindLabel(selectedRow.kind)}</dd>
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
						<dd><code class="task-code">{selectedRow.toolRunId}</code></dd>
					</div>
					{#if selectedRow.kind === 'background'}
						<div>
							<dt>耗时</dt>
							<dd>{selectedRow.timing}</dd>
						</div>
					{/if}
					{#if selectedRow.kind === 'background' && selectedRow.details.command}
						<div>
							<dt>执行命令</dt>
							<dd><code class="task-command">{selectedRow.details.command}</code></dd>
						</div>
					{/if}
					{#if selectedRow.kind === 'scheduled'}
						<div>
							<dt>执行时间</dt>
							<dd>{selectedRow.timing}</dd>
						</div>
					{/if}
				</dl>

				{#if selectedRow.details.body}
					<section class="task-dialog-section">
						<h4>任务内容</h4>
						<p class="task-detail-copy">{selectedRow.details.body}</p>
					</section>
				{/if}
				{#if selectedRow.details.output}
					<section class="task-dialog-section">
						<h4>任务输出</h4>
						<p class="task-detail-copy">{selectedRow.details.output}</p>
					</section>
				{/if}
				{#if selectedRow.details.error}
					<section class="task-dialog-section">
						<h4>错误详情</h4>
						<p class="task-detail-copy">{selectedRow.details.error}</p>
					</section>
				{/if}
				<div class="task-actions">
					{#if selectedRow.kind === 'background' && selectedRow.status === 'running'}
						<MaterialButton
							variant="danger"
							label="停止任务"
							onclick={() => onCancel?.(selectedRow.toolRunId, 'background')}
						/>
					{/if}
					{#if selectedRow.kind === 'scheduled' && (selectedRow.status === 'waiting' || selectedRow.status === 'running')}
						<MaterialButton
							variant="danger"
							label="取消定时任务"
							onclick={() => onCancel?.(selectedRow.toolRunId, 'scheduled')}
						/>
					{/if}
					{#if selectedRow.sessionId}
						<MaterialButton
							variant="outlined"
							label="打开来源会话"
							onclick={() => {
								const sessionId = selectedRow.sessionId;
								if (sessionId) onOpenSession?.(sessionId);
							}}
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
		flex: 1 1 var(--md-comp-settings-control-width);
		min-width: 0;
	}
	.task-filter {
		flex: 0 1 var(--md-comp-control-compact-width);
		min-width: 0;
	}
	.task-list-panel {
		min-width: 0;
		/* The workspace-level frame is supplied by WorkspaceSurface. Keep this
		 * region as content rhythm so task and history views share one outer
		 * surface instead of stacking two competing cards. */
	}
	:global(.task-count) {
		flex: 0 0 auto;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
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
	.task-card-header :global(.md-badge),
	:global(.task-dialog-type-row .md-badge) {
		flex: 0 0 auto;
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
