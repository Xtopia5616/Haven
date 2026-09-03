<script>
	import MaterialBadge from '$lib/MaterialBadge.svelte';
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
		onStatusFilterChange = () => {},
		onOpenDateFilter = () => {},
		onToggleSelectAll = () => {},
		onToggleSelect = () => {},
		onResume = () => {},
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
	/** @param {string} value */
	function handleStatusChange(value) {
		onStatusFilterChange(value);
	}
</script>

<div class="history-view">
	<div class="filter-bar md-toolbar">
		<input
			class="md-input"
			type="text"
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
				options={statusOptions}
				onChange={handleStatusChange}
			/>
			<button class="md-btn md-btn--outlined" onclick={() => onOpenDateFilter()}
				>{#if startDate || endDate}日期：{startDate ? startDate.replace(/-/g, '/') : '…'} ~ {endDate
						? endDate.replace(/-/g, '/')
						: '…'}{:else}日期筛选{/if}</button
			>
		</div>
	</div>

	{#if selectMode && sessions.length > 0}
		<div class="select-bar md-toolbar">
			<button class="select-all-row" onclick={() => onToggleSelectAll()}
				><div
					class="md-checkbox-static"
					class:checked={selectedIds.size === sessions.length}
				></div>
				<span>全选（{sessions.length}）</span></button
			>
		</div>
	{/if}
	{#if sessions.length === 0}
		<AsyncState
			state={loading ? 'loading' : 'empty'}
			title={loading ? '正在加载会话' : '暂无会话'}
			message={loading ? '会话记录加载完成后会显示在这里。' : '开始一段对话后，会话记录会自动保存在这里。'}
		/>
	{:else}
		<div class="session-list">
			{#each sessions as session (session.id)}
				{#if selectMode}
					<button
						class="session-item session-item-btn"
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
										text={session.status}
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
					<div
						class="session-item"
						class:selected={selectedIds.has(session.id)}
						role="button"
						tabindex="0"
						onclick={() => onResume(session)}
						onkeydown={(event) => {
							if (event.key === 'Enter' || event.key === ' ') {
								event.preventDefault();
								onResume(session);
							}
						}}
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
										onclick={(event) => event.stopPropagation()}
										autofocus
										autocomplete="off"
									/>
								{:else}
									<span
										class="session-title"
										onclick={(event) => {
											event.stopPropagation();
											onStartEdit(session);
										}}
										onkeydown={(event) => {
											if (event.key === 'Enter') {
												event.stopPropagation();
												onStartEdit(session);
											}
										}}
										role="button"
										tabindex="0"
										>{displayTitle(session)}<svg
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
										></span
									>
								{/if}
								<MaterialBadge
									variant={statusVariant(session.status)}
									text={session.status}
								/>
							</div>
							{#if session.transcript}<div class="session-message">
									"{session.transcript}"
								</div>{/if}
							<div class="session-meta">
								<span class="meta-date"
									>{formatMessageTime(session.created_at)}</span
								><button
									class="md-btn md-btn--xs md-btn--text delete-btn-meta"
									onclick={(event) => {
										event.stopPropagation();
										onDeleteRequest(session);
									}}>删除</button
								>
							</div>
						</div>
					</div>
				{/if}
			{/each}
		</div>
		{#if hasMore}<div class="load-more-row">
				<button
					class="md-btn md-btn--outlined"
					onclick={() => onLoadMore()}
					disabled={loading}>{loading ? '加载中…' : '加载更多'}</button
				>
			</div>{/if}
	{/if}
</div>

<style>
	.filter-bar {
		display: flex;
		align-items: center;
		min-width: 0;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-lg);
		padding: var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-low);
	}
	.filter-bar > .md-input {
		flex: 1;
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
	.filter-controls .md-btn--outlined {
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
	.select-all-row {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		background: none;
		border: none;
		font-family: inherit;
		cursor: pointer;
		padding: 0;
	}
	.session-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
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
		padding: var(--md-sys-space-lg);
		cursor: pointer;
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
	.session-title {
		font-size: 14px;
		font-weight: 600;
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
	.session-title:hover .title-edit-icon {
		opacity: 1;
	}
	.title-edit-icon {
		opacity: 0;
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: opacity 0.15s;
	}
	.title-input {
		font-size: 14px;
		font-weight: 600;
		padding: 2px 6px;
		width: 280px;
	}
	.session-message {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
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
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.75;
	}
	.meta-date {
		font-family: var(--md-sys-typescale-mono);
		font-size: 10px;
	}
	.delete-btn-meta {
		margin-left: auto;
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
		.filter-controls :global(.md-select-container),
		.filter-controls .md-btn--outlined {
			width: 100%;
		}
		.session-item-main {
			padding: var(--md-sys-space-md);
		}
	}
</style>
