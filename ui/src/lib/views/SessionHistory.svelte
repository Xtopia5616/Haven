<script lang="ts">
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import Icon from '$lib/Icon.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import AsyncState from '$lib/AsyncState.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import HistoryListGroup from '$lib/HistoryListGroup.svelte';
	import type { SessionHistoryRow } from '$lib/contracts/sessionHistory.ts';
	import {
		SESSION_HISTORY_STATUS_FILTER_INPUT_VALUES,
		type SessionHistoryStatusFilterInput,
	} from '$lib/contracts/generatedCommands.ts';

	type SessionHistoryStatusSelection = '' | SessionHistoryStatusFilterInput;

	interface Props {
		sessions?: SessionHistoryRow[];
		searchQuery?: string;
		totalCount?: number;
		statusFilter?: SessionHistoryStatusSelection;
		statusOptions?: Array<{ value: SessionHistoryStatusSelection; label: string }>;
		startDate?: string;
		endDate?: string;
		loading?: boolean;
		hasMore?: boolean;
		editingTitle?: string | null;
		renameValue?: string;
		onSearchQueryChange?: (value: string) => void;
		onSearchInput?: () => void;
		onClearFilters?: () => void;
		onStatusFilterChange?: (value: SessionHistoryStatusSelection) => void;
		onOpenDateFilter?: () => void;
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
			status: SessionHistoryRow['status'],
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
		loading = false,
		hasMore = false,
		editingTitle = null,
		renameValue = '',
		onSearchQueryChange = () => {},
		onSearchInput = () => {},
		onClearFilters = () => {},
		onStatusFilterChange = () => {},
		onOpenDateFilter = () => {},
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
	const statusLabels: Record<SessionHistoryRow['status'], string> = {
		pending: '排队中',
		running: '运行中',
		paused: '已暂停',
		completed: '已完成',
		error: '错误',
	};
	function sessionStatusLabel(status: SessionHistoryRow['status']) {
		return statusLabels[status];
	}
	function isSessionHistoryStatusFilterInput(
		value: string,
	): value is SessionHistoryStatusFilterInput {
		return SESSION_HISTORY_STATUS_FILTER_INPUT_VALUES.some((candidate) => candidate === value);
	}
	function handleStatusChange(value: string) {
		if (value === '') {
			onStatusFilterChange('');
		} else if (isSessionHistoryStatusFilterInput(value)) {
			onStatusFilterChange(value);
		}
	}
	function handleSessionKeydown(event: KeyboardEvent, session: SessionHistoryRow) {
		if (event.target !== event.currentTarget) return;
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			handleSessionOpen(session);
		}
	}
	function handleSessionOpen(session: SessionHistoryRow) {
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
	const sessionGroups = $derived.by(() => {
		const ongoing = sessions.filter(
			(session) => !['completed', 'error'].includes(session.status),
		);
		const ended = sessions.filter((session) => ['completed', 'error'].includes(session.status));
		return [
			{ id: 'in-progress', title: '进行中', sessions: ongoing },
			{ id: 'ended', title: '已结束', sessions: ended },
		].filter((group) => group.sessions.length > 0);
	});
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
				{#each sessionGroups as group (group.id)}
					<HistoryListGroup
						id={group.id}
						title={group.title}
						count={group.sessions.length}
					>
						{#snippet children()}
							{#each group.sessions as session (session.id)}
								<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
								<article
									class="session-item workspace-item-card motion-list-item"
									aria-label={`打开并继续会话：${displayTitle(session)}`}
									role="button"
									tabindex="0"
									onclick={() => handleSessionOpen(session)}
									onkeydown={(event) => handleSessionKeydown(event, session)}
									oncontextmenu={(event) =>
										handleSessionContextMenu(event, session)}
								>
									<div class="session-item-main workspace-item-card-main">
										<div class="workspace-item-card-header">
											<span
												class="workspace-item-card-kind"
												data-tone={statusVariant(session.status)}
											>
												<span
													class="workspace-item-card-indicator"
													data-tone={statusVariant(session.status)}
													aria-hidden="true"
												></span>
												会话
											</span>
											<MaterialBadge
												variant={statusVariant(session.status)}
												text={sessionStatusLabel(session.status)}
											/>
										</div>
										<div class="session-title-row">
											{#if editingTitle === session.id}
												<!-- svelte-ignore a11y_autofocus -->
												<input
													type="text"
													class="md-input title-input"
													value={renameValue}
													oninput={(event) =>
														onRenameValueChange(
															event.currentTarget.value,
														)}
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
														<span class="workspace-item-card-title"
															>{displayTitle(session)}</span
														><Icon
															name="edit"
															size={14}
															className="title-edit-icon"
														/>
													{/snippet}
												</MaterialButton>
											{/if}
										</div>
										{#if session.input_text}<div
												class="workspace-item-card-summary"
											>
												{session.input_text}
											</div>{/if}
										<div class="session-meta workspace-item-card-meta">
											<span class="meta-date"
												>{formatMessageTime(session.created_at)}</span
											>
										</div>
										<div class="session-footer workspace-item-card-footer">
											<span
												class="session-open-hint workspace-item-card-open"
												aria-hidden="true">打开</span
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
							{/each}
						{/snippet}
					</HistoryListGroup>
				{/each}
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
	.session-title-row {
		display: flex;
		align-items: stretch;
		min-width: 0;
	}
	:global(.md-btn.session-title) {
		position: relative;
		height: auto;
		width: 100%;
		min-width: 0;
		padding: 0;
		text-align: left;
		justify-content: flex-start;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		overflow: hidden;
		flex: 1;
	}
	:global(.md-btn.session-title .workspace-item-card-title) {
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
		font-size: var(--md-sys-typescale-title-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-medium-line-height);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
	}
	.meta-date {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.load-more-row {
		display: flex;
		justify-content: center;
		padding: var(--md-sys-space-lg) 0;
	}
	@media (max-width: 700px) {
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
