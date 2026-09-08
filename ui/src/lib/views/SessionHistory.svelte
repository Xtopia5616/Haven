<script>
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import AsyncState from '$lib/AsyncState.svelte';

	/** Session history tab. Loading, resume and store updates remain in the parent. */
	let {
		sessions = [],
		searchQuery = '',
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
		statusVariant = () => 'neutral',
		formatMessageTime = (/** @type {string} */ value) => value,
	} = $props();
	/** @type {Record<string, string>} */
	const statusLabels = {
		pending: '排队中',
		running: '运行中',
		paused: '已暂停',
		paused_awaiting_answer: '等待回答',
		paused_awaiting_confirm: '等待确认',
		completed: '已完成',
		error: '错误',
		failed: '失败',
	};
	/** @param {string} status */
	function sessionStatusLabel(status) {
		return statusLabels[status] || status;
	}
	/** @param {string} value */
	function handleStatusChange(value) {
		onStatusFilterChange(value);
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
	</div>

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
		<div class="session-list">
			{#each sessions as session (session.id)}
				{#if selectMode}
					<button
						class="session-item session-item-btn motion-list-item"
						class:selected={selectedIds.has(session.id)}
						onclick={() => onToggleSelect(session.id)}
					>
						<div class="session-item-main">
							<div class="session-top-row">
								<div class="select-checkbox">
									<div
										class="md-checkbox-static"
										class:checked={selectedIds.has(session.id)}
									></div>
								</div>
								<div class="session-title-row">
									<span class="session-title">{displayTitle(session)}</span
									><MaterialBadge
										variant={statusVariant(session.status)}
										text={sessionStatusLabel(session.status)}
									/>
								</div>
							</div>
							{#if session.transcript}<div class="session-message">
									"{session.transcript}"
								</div>{/if}
							<div class="session-meta">
								<span class="meta-date"
									>{formatMessageTime(session.created_at)}</span
								>
							</div>
						</div>
					</button>
				{:else}
					<article
						class="session-item motion-list-item"
						class:selected={selectedIds.has(session.id)}
						aria-label={`会话：${displayTitle(session)}`}
						oncontextmenu={(event) => onContextMenu(event, session)}
					>
						<div class="session-item-main">
							<div class="session-title-row">
								{#if editingTitle === session.id}
									<!-- svelte-ignore a11y_autofocus -->
									<input
										type="text"
										class="md-input title-input"
										value={renameValue}
										oninput={(event) =>
											onRenameValueChange(event.currentTarget.value)}
										onkeydown={(event) => onRenameKeydown(event, session.id)}
										onblur={() => onSaveTitle(session.id)}
										autofocus
										autocomplete="off"
									/>
								{:else}
					<MaterialButton
						variant="text"
						className="session-title"
						ariaLabel={`重命名${displayTitle(session)}`}
						onclick={() => onStartEdit(session)}
					>
						{#snippet children()}
							{displayTitle(session)}<svg
								class="title-edit-icon"
								width="14"
								height="14"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"
								><path
									d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"
								/></svg
							>
						{/snippet}
					</MaterialButton>
								{/if}
								<MaterialBadge
									variant={statusVariant(session.status)}
									text={sessionStatusLabel(session.status)}
								/>
							</div>
							{#if session.transcript}<div class="session-message">
									"{session.transcript}"
								</div>{/if}
							<div class="session-meta">
								<span class="meta-date"
									>{formatMessageTime(session.created_at)}</span
								><span class="session-actions">
									<MaterialButton
										variant="tonal"
										className="open-session-btn"
										label="打开"
										onclick={() => onResume(session)}
									/>
									<MaterialButton
										variant="text"
										className="delete-btn-meta"
										label="删除"
										onclick={() => {
											onDeleteRequest(session);
										}}
									/>
								</span>
							</div>
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
	{/if}
</div>

<style>
	.filter-bar {
		margin-bottom: var(--md-sys-space-lg);
	}
	.filter-controls {
		display: flex;
		align-items: center;
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
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.session-item {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		transition:
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
		outline: none;
	}
	.session-item:hover {
		background: var(--md-sys-color-surface-container);
		border-color: var(--md-sys-color-outline);
		box-shadow: var(--md-sys-elevation-1);
	}
	.session-item:focus-visible {
		border-color: var(--md-sys-color-primary);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--md-sys-color-primary) 30%, transparent);
	}
	.session-item.selected {
		background: var(--md-sys-color-primary-container);
		border-color: var(--md-sys-color-primary);
	}
	.session-item-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md);
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
	.title-edit-icon {
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
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		line-height: var(--md-sys-typescale-body-small-line-height);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
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
	.session-actions {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		margin-left: auto;
	}
	.session-actions :global(.md-btn) {
		min-width: 0;
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
	@media (max-width: 700px) {
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
		.session-item-main {
			padding: var(--md-sys-space-md);
		}
		.session-meta {
			flex-wrap: wrap;
		}
		.session-actions {
			width: 100%;
			margin-left: 0;
		}
	}
</style>
