<script lang="ts">
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import Icon from '$lib/Icon.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import AsyncState from '$lib/AsyncState.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import type { SessionHistoryRow } from '$lib/contracts/sessionHistory.ts';

	interface Props {
		sessions?: SessionHistoryRow[];
		searchQuery?: string;
		totalCount?: number;
		statusFilter?: string;
		statusOptions?: Array<{ value: string; label: string }>;
		startDate?: string;
		endDate?: string;
		selectMode?: boolean;
		selectedIds?: Set<string>;
		loading?: boolean;
		hasMore?: boolean;
		editingTitle?: string | null;
		renameValue?: string;
		onSearchQueryChange?: (value: string) => void;
		onSearchInput?: () => void;
		onClearFilters?: () => void;
		onStatusFilterChange?: (value: string) => void;
		onOpenDateFilter?: () => void;
		onToggleSelectAll?: () => void;
		onToggleSelect?: (sessionId: string) => void;
		onEnterSelectMode?: () => void;
		onCancelSelectMode?: () => void;
		onExportSelected?: () => void;
		onOpenClearDialog?: () => void;
		onResume?: (session: SessionHistoryRow) => void;
		onNewSession?: () => void;
		onStartEdit?: (session: SessionHistoryRow) => void;
		onRenameValueChange?: (value: string) => void;
		onRenameKeydown?: (event: KeyboardEvent, sessionId: string) => void;
		onSaveTitle?: (sessionId: string) => void;
		onContextMenu?: (event: MouseEvent, session: SessionHistoryRow) => void;
		onDeleteRequest?: (session: SessionHistoryRow) => void;
		onLoadMore?: () => void;
		displayTitle?: (session: SessionHistoryRow) => string;
		statusVariant?: (
			status: string,
		) => 'default' | 'primary' | 'secondary' | 'success' | 'warning' | 'error';
		formatMessageTime?: (value: string) => string;
	}

	/** Session history tab. Loading, resume and store updates remain in the parent. */
	let {
		sessions = [],
		searchQuery = '',
		totalCount = 0,
		statusFilter = '',
		statusOptions = [],
		startDate = '',
		endDate = '',
		selectMode = false,
		selectedIds = new Set(),
		loading = false,
		hasMore = false,
		editingTitle = null,
		renameValue = '',
		onSearchQueryChange = () => {},
		onSearchInput = () => {},
		onClearFilters = () => {},
		onStatusFilterChange = () => {},
		onOpenDateFilter = () => {},
		onToggleSelectAll = () => {},
		onToggleSelect = () => {},
		onEnterSelectMode = () => {},
		onCancelSelectMode = () => {},
		onExportSelected = () => {},
		onOpenClearDialog = () => {},
		onResume = () => {},
		onNewSession = () => {},
		onStartEdit = () => {},
		onRenameValueChange = () => {},
		onRenameKeydown = () => {},
		onSaveTitle = () => {},
		onContextMenu = () => {},
		onDeleteRequest = () => {},
		onLoadMore = () => {},
		displayTitle = () => '未命名会话',
		statusVariant = () => 'default',
		formatMessageTime = (value) => value,
	}: Props = $props();
	const statusLabels: Record<string, string> = {
		pending: '排队中',
		running: '运行中',
		paused: '已暂停',
		completed: '已完成',
		error: '错误',
	};
	function sessionStatusLabel(status: string) {
		return statusLabels[status] || status;
	}
	function handleStatusChange(value: string) {
		onStatusFilterChange(value);
	}
	function handleSessionKeydown(event: KeyboardEvent, session: SessionHistoryRow) {
		if (event.target !== event.currentTarget) return;
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			handleSessionOpen(session);
		}
	}
	function handleSessionOpen(session: SessionHistoryRow) {
		if (selectMode) {
			onToggleSelect(session.id);
			return;
		}
		onResume(session);
	}
	function handleSessionContextMenu(event: MouseEvent, session: SessionHistoryRow) {
		const target = event.target instanceof Element ? event.target : null;
		if (target?.closest('input, textarea, select, [contenteditable="true"]')) return;
		onContextMenu(event, session);
	}
	const hasFilters = $derived(
		Boolean(searchQuery.trim() || statusFilter || startDate || endDate),
	);
</script>

<div class="history-view">
	<div class="filter-bar workspace-filter-bar" role="search" aria-label="筛选会话">
		<input
			class="md-input"
			type="text"
			aria-label="搜索会话"
			placeholder="搜索会话"
			value={searchQuery}
			oninput={(event) => {
				onSearchQueryChange(event.currentTarget.value);
				onSearchInput();
			}}
			autocomplete="off"
		/>
		<div class="filter-controls">
			<MaterialSelect
				value={statusFilter}
				ariaLabel="会话状态"
				options={statusOptions}
				onChange={handleStatusChange}
			/>
			<MaterialButton
				variant="outlined"
				label={startDate || endDate
					? `日期：${startDate ? startDate.replace(/-/g, '/') : '…'} ~ ${endDate ? endDate.replace(/-/g, '/') : '…'}`
					: '日期筛选'}
				onclick={() => onOpenDateFilter()}
			/>
			{#if hasFilters}
				<MaterialButton variant="text" label="清除" onclick={() => onClearFilters()} />
			{/if}
		</div>
		<CountChip count={totalCount} label="条历史" live />
	</div>

	{#if sessions.length > 0}
		<div class="history-actions" aria-label="历史操作">
			{#if selectMode}
				<MaterialButton
					variant="filled"
					label={`导出选中（${selectedIds.size}）`}
					onclick={() => onExportSelected()}
					disabled={selectedIds.size === 0}
				/>
				<MaterialButton variant="text" label="取消" onclick={() => onCancelSelectMode()} />
			{:else}
				<MaterialButton
					variant="outlined"
					label="导出"
					onclick={() => onEnterSelectMode()}
				/>
				<MaterialButton
					variant="danger"
					label="清空会话"
					onclick={() => onOpenClearDialog()}
				/>
			{/if}
		</div>
	{/if}

	{#if selectMode && sessions.length > 0}
		<div class="select-bar md-toolbar">
			<MaterialButton
				variant="text"
				className="select-all-row"
				ariaPressed={selectedIds.size === sessions.length}
				onclick={() => onToggleSelectAll()}
			>
				{#snippet children()}
					<span
						class="md-checkbox-static"
						class:checked={selectedIds.size === sessions.length}
					></span>
					<span>全选（{sessions.length}）</span>
				{/snippet}
			</MaterialButton>
		</div>
	{/if}
	{#if sessions.length === 0}
		<AsyncState
			state={loading ? 'loading' : 'empty'}
			title={loading ? '正在加载会话' : '暂无会话'}
			message={loading
				? '会话记录加载完成后会显示在这里。'
				: '开始一段对话后，会话记录会自动保存在这里。'}
			actionLabel={loading ? '' : '开始新会话'}
			onAction={onNewSession}
		/>
	{:else}
		<div class="history-results">
			<div class="history-list-column">
				<div class="session-list">
					{#each sessions as session (session.id)}
						{#if selectMode}
							<button
								class="session-item session-item-btn workspace-item-card motion-list-item"
								class:selected={selectedIds.has(session.id)}
								aria-pressed={selectedIds.has(session.id)}
								onclick={() => onToggleSelect(session.id)}
							>
								<div class="session-item-main workspace-item-card-main">
									<div class="session-top-row">
										<div class="select-checkbox">
											<div
												class="md-checkbox-static"
												class:checked={selectedIds.has(session.id)}
											></div>
										</div>
										<div class="session-title-row">
											<span class="session-title"
												>{displayTitle(session)}</span
											><MaterialBadge
												variant={statusVariant(session.status)}
												text={sessionStatusLabel(session.status)}
											/>
										</div>
									</div>
									{#if session.input_text}<div class="session-message">
											"{session.input_text}"
										</div>{/if}
									<div class="session-meta workspace-item-card-meta">
										<span class="meta-date"
											>{formatMessageTime(session.created_at)}</span
										>
									</div>
								</div>
							</button>
						{:else}
							<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
							<article
								class="session-item workspace-item-card motion-list-item"
								aria-label={`打开并继续会话：${displayTitle(session)}`}
								role="button"
								tabindex="0"
								onclick={() => handleSessionOpen(session)}
								onkeydown={(event) => handleSessionKeydown(event, session)}
								oncontextmenu={(event) => handleSessionContextMenu(event, session)}
							>
								<div class="session-item-main workspace-item-card-main">
									<div class="session-title-row workspace-item-card-header">
										{#if editingTitle === session.id}
											<!-- svelte-ignore a11y_autofocus -->
											<input
												type="text"
												class="md-input title-input"
												value={renameValue}
												oninput={(event) =>
													onRenameValueChange(event.currentTarget.value)}
												onkeydown={(event) =>
													onRenameKeydown(event, session.id)}
												onclick={(event) => event.stopPropagation()}
												onblur={() => onSaveTitle(session.id)}
												autofocus
												autocomplete="off"
											/>
										{:else}
											<MaterialButton
												variant="text"
												className="session-title"
												ariaLabel={`重命名${displayTitle(session)}`}
												onclick={(event) => {
													event.stopPropagation();
													onStartEdit(session);
												}}
											>
												{#snippet children()}
													{displayTitle(session)}<Icon
														name="edit"
														size={14}
														className="title-edit-icon"
													/>
												{/snippet}
											</MaterialButton>
										{/if}
										<MaterialBadge
											variant={statusVariant(session.status)}
											text={sessionStatusLabel(session.status)}
										/>
									</div>
									{#if session.input_text}<div class="session-message">
											"{session.input_text}"
										</div>{/if}
									<div class="session-footer workspace-item-card-footer">
										<span class="meta-date"
											>{formatMessageTime(session.created_at)}</span
										>
										<span
											class="session-open-hint workspace-item-card-open"
											aria-hidden="true"
											>打开</span
										>
									</div>
								</div>
								<div class="session-actions workspace-item-card-actions">
									<MaterialButton
										variant="text"
										className="delete-btn-meta"
										label="删除"
										onclick={(event) => {
											event.stopPropagation();
											onDeleteRequest(session);
										}}
									/>
								</div>
							</article>
						{/if}
					{/each}
				</div>
				{#if hasMore}<div class="load-more-row">
						<MaterialButton
							variant="outlined"
							label={loading ? '加载中…' : '加载更多'}
							onclick={() => onLoadMore()}
							disabled={loading}
						/>
				</div>{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.history-view {
		min-width: 0;
		container: session-history / inline-size;
	}
	.history-results,
	.history-list-column {
		display: contents;
	}
	.filter-bar {
		margin-bottom: var(--md-sys-space-lg);
	}
	.history-actions {
		display: flex;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-lg);
	}
	.filter-controls {
		display: flex;
		align-items: center;
		flex: 0 0 auto;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.filter-controls :global(.md-select-container) {
		width: 140px;
		flex-shrink: 0;
	}
	.filter-controls :global(.md-btn--outlined) {
		width: 120px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		display: inline-block;
		flex-shrink: 0;
	}
	.select-bar {
		margin-bottom: var(--md-sys-space-md);
	}
	:global(.md-btn.select-all-row) {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		height: auto;
		min-width: 0;
		padding: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 360px), 1fr));
		gap: var(--md-sys-space-sm);
		min-width: 0;
		padding: var(--md-sys-space-xs);
	}
	.session-item-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.session-top-row,
	.session-title-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.session-title-row {
		gap: var(--md-sys-space-sm);
		flex: 1;
		min-width: 0;
	}
	:global(.md-btn.session-title) {
		position: relative;
		height: auto;
		min-width: 0;
		padding: 0;
		text-align: left;
		justify-content: flex-start;
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
	}
	:global(.md-btn.session-title:focus-visible) {
		border-radius: var(--md-sys-shape-extra-small);
		outline: none;
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 30%, transparent);
	}
	:global(.md-btn.session-title:hover .title-edit-icon) {
		opacity: 1;
	}
	:global(.title-edit-icon) {
		opacity: 0.45;
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.title-input {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		width: 280px;
	}
	.session-message {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		min-width: 0;
		overflow: hidden;
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		line-height: var(--md-sys-typescale-body-small-line-height);
		overflow-wrap: anywhere;
	}
	.session-meta {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		line-height: var(--md-sys-typescale-label-small-line-height);
		opacity: 0.75;
	}
	.session-footer {
		min-height: 28px;
	}
	.session-footer .session-open-hint {
		margin-left: auto;
	}
	.meta-date {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.session-item-btn {
		width: 100%;
		text-align: left;
		font-family: inherit;
		cursor: pointer;
	}
	.select-checkbox {
		flex-shrink: 0;
		display: flex;
		align-items: center;
	}
	.md-checkbox-static {
		width: 18px;
		height: 18px;
		border: 2px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-extra-small);
		background: transparent;
		position: relative;
		flex-shrink: 0;
	}
	.md-checkbox-static.checked {
		background: var(--md-sys-color-primary);
		border-color: var(--md-sys-color-primary);
	}
	.md-checkbox-static.checked::after {
		content: '';
		position: absolute;
		left: 50%;
		top: 50%;
		width: 5px;
		height: 9px;
		border: solid var(--md-sys-color-on-primary);
		border-width: 0 2px 2px 0;
		transform: translate(-50%, -60%) rotate(45deg);
	}
	.load-more-row {
		display: flex;
		justify-content: center;
		padding: var(--md-sys-space-lg) 0;
	}
	@media (min-width: 840px) {
		.session-list {
			grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
			align-items: start;
			gap: var(--md-sys-space-md);
		}
	}
	@media (max-width: 700px) {
		.history-actions {
			justify-content: stretch;
		}
		.history-actions :global(.md-btn) {
			flex: 1 1 0;
			min-width: 0;
		}
		.session-item-main {
			padding: var(--md-sys-space-md);
		}
		.session-meta {
			flex-wrap: wrap;
		}
	}
	@container session-history (max-width: 700px) {
		.filter-bar,
		.filter-controls {
			align-items: stretch;
			flex-direction: column;
		}
		.filter-bar > .md-input {
			flex: none;
		}
		.filter-controls :global(.md-select-container),
		.filter-controls :global(.md-btn--outlined) {
			width: 100%;
		}
		.filter-bar > :global(.count-chip) {
			align-self: flex-start;
		}
	}
</style>
