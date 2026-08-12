<script>
	let sessions = $state([]);
	let searchQuery = $state('');
	let searchTimer;
	let deleteTarget = $state(null);
	let showClearDialog = $state(false);
	let selectMode = $state(false);
	let selectedIds = $state(new Set());
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	let loadHistorySeq = 0;
	const PAGE_SIZE = 50;

	let statusFilter = $state('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);

	let editingTitle = $state(null); // { sessionId, value }
	let renameValue = $state('');

	// Right-click context menu on a history item (open / rename / export / delete)
	let ctxMenu = $state({ open: false, x: 0, y: 0, session: null });

	const todayISO = $derived.by(() => {
		const n = new Date();
		return `${n.getFullYear()}-${String(n.getMonth() + 1).padStart(2, '0')}-${String(n.getDate()).padStart(2, '0')}`;
	});

	import logger from '$lib/logger.js';
	import { buildReviewMessages, mergeLiveStreaming } from '$lib/reviewMessages.js';
	import { formatMessageTime } from '$lib/stores.js';
	import { statusVariant } from '$lib/sessionStatus.js';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { invoke } from '$lib/tauri.js';
	import { sessionMessagesStore, updateSessionMessages, clearSessionMessages, reviewTargetStore, activeSessionIdStore, restoreSessionTokenStats, restoreSessionLlmUsage, addNotification } from '$lib/stores.js';
	import { registerOne } from '$lib/events.js';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';

	const statusOptions = [
		{ value: '', label: 'All' },
		{ value: 'completed', label: 'Completed' },
		{ value: 'paused', label: 'Paused' },
		{ value: 'error', label: 'Error' },
		{ value: 'failed', label: 'Failed' },
	];

	let unlistenTitleUpdate = null;

	onMount(async () => {
		await loadHistory();
		const reg = await registerOne('session:title-updated', (event) => {
			const { session_id, title } = event.payload;
			sessions = sessions.map(t => t.id === session_id ? { ...t, title } : t);
		}, { tag: 'history' });
		unlistenTitleUpdate = reg;
	});

	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
		if (unlistenTitleUpdate) unlistenTitleUpdate.dispose();
	});

	function filterParams(extra) {
		return {
			query: searchQuery || null,
			status: statusFilter || null,
			startDate: startDate || null,
			endDate: endDate || null,
			...extra,
		};
	}

	async function loadHistory() {
		const seq = ++loadHistorySeq;
		loading = true;
		try {
			const results = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset: 0 }));
			// Stale response guard: a newer loadHistory call superseded this one.
			if (seq !== loadHistorySeq) return;
			sessions = results || [];
			totalCount = sessions.length;
			offset = PAGE_SIZE;
			hasMore = sessions.length >= PAGE_SIZE;
		} catch {
			if (seq !== loadHistorySeq) return;
			sessions = [];
			totalCount = 0;
			hasMore = false;
			addNotification('加载历史记录失败', 'error', 3000);
		}
		if (seq === loadHistorySeq) loading = false;
	}

	async function loadMore() {
		if (loading || !hasMore) return;
		const seq = loadHistorySeq;
		loading = true;
		try {
			const more = await invoke('search_history_filtered', filterParams({ limit: PAGE_SIZE, offset }));
			// Stale guard: a filter/search change superseded this page fetch.
			if (seq !== loadHistorySeq) return;
			if (more && more.length > 0) {
				sessions = [...sessions, ...more];
				offset += more.length;
				hasMore = more.length >= PAGE_SIZE;
				totalCount = sessions.length;
			} else {
				hasMore = false;
			}
		} catch {
			if (seq !== loadHistorySeq) return;
			hasMore = false;
			addNotification('加载更多历史记录失败', 'error', 3000);
		}
		if (seq === loadHistorySeq) loading = false;
	}

	async function handleSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(loadHistory, 300);
	}

	function handleFilterChange() {
		loadHistory();
	}

	async function reviewSession(session) {
		try {
			await invoke('reopen_session', { sessionId: session.id });
			const result = await invoke('get_session_for_review', { sessionId: session.id });
			const dbMessages = buildReviewMessages(result);
			// Atomically merge DB messages with any in-memory streaming messages
			// that arrived concurrently (e.g. from background session streaming).
			updateSessionMessages(session.id, (existing) =>
				mergeLiveStreaming(dbMessages, existing)
			);
			restoreSessionTokenStats(session.id, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(session.id, result.llm_usage);
			reviewTargetStore.set({ sessionId: session.id, summary: session.input_text, title: session.title, wasError: session.status === 'error' || session.status === 'failed' });
			await goto('/');
		} catch (e) {
			addNotification(`加载会话详情失败: ${e}`, 'error', 4000);
		}
	}


	async function deleteSession(sessionId) {
		try {
			await invoke('delete_session', { sessionId });
			sessions = sessions.filter((t) => t.id !== sessionId);
			totalCount = sessions.length;
			// If the deleted session was the active one, reset conversation state.
			const current = get(activeSessionIdStore);
			if (current === sessionId) {
				activeSessionIdStore.set(null);
				clearSessionMessages(sessionId);
			}
			addNotification('会话已删除', 'success', 2000);
		} catch (e) {
			addNotification(`删除失败: ${e}`, 'error', 4000);
		}
		deleteTarget = null;
	}

	async function clearHistory() {
		try {
			const count = await invoke('clear_history');
			sessions = [];
			totalCount = 0;
			hasMore = false;
			activeSessionIdStore.set(null);
			sessionMessagesStore.set({});
			addNotification(`已清空 ${count} 条历史记录`, 'success', 3000);
		} catch {
			addNotification('清空历史记录失败', 'error', 4000);
		}
		showClearDialog = false;
	}

	function enterSelectMode() {
		selectMode = true;
		selectedIds = new Set();
	}

	function cancelSelectMode() {
		selectMode = false;
		selectedIds = new Set();
	}

	function toggleSelect(sessionId) {
		const next = new Set(selectedIds);
		if (next.has(sessionId)) {
			next.delete(sessionId);
		} else {
			next.add(sessionId);
		}
		selectedIds = next;
	}

	function toggleSelectAll() {
		if (selectedIds.size === sessions.length) {
			selectedIds = new Set();
		} else {
			selectedIds = new Set(sessions.map((t) => t.id));
		}
	}


	function displayTitle(session) {
		if (session.title) return session.title;
		const text = session.input_text || '';
		const m = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		return (m ? m[0].trim() : text.trim()) || 'Untitled';
	}

	function startEdit(session) {
		editingTitle = session.id;
		if (session.title) {
			renameValue = session.title;
		} else {
			const text = session.input_text || '';
			const m = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
			renameValue = m ? m[0].trim() : text.trim();
		}
	}

	function cancelEdit() {
		editingTitle = null;
		renameValue = '';
	}

	async function saveTitle(sessionId) {
		const value = renameValue.trim();
		if (!value) { cancelEdit(); return; }
		try {
			await invoke('update_session_title', { sessionId, title: value });
			const t = sessions.find(t => t.id === sessionId);
			if (t) t.title = value;
		} catch (e) {
			addNotification(`重命名失败: ${e}`, 'error', 3000);
		}
		cancelEdit();
	}

	function handleRenameKeydown(e, sessionId) {
		if (e.key === 'Enter') { e.preventDefault(); saveTitle(sessionId); }
		else if (e.key === 'Escape') { cancelEdit(); }
	}

	function downloadSessions(sessionsToExport) {
		const json = JSON.stringify(
			{
				exported_at: new Date().toISOString(),
				count: sessionsToExport.length,
				sessions: sessionsToExport,
			},
			null,
			2,
		);
		const blob = new Blob([json], { type: 'application/json' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = `haven-history-${new Date().toISOString().slice(0, 10)}.json`;
		a.click();
		URL.revokeObjectURL(url);
	}

	function openCtxMenu(e, session) {
		e.preventDefault();
		e.stopPropagation();
		ctxMenu = { open: true, x: e.clientX, y: e.clientY, session };
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, session: null };
	}

	let ctxMenuItems = $derived.by(() => {
		const session = ctxMenu.session;
		if (!session) return [];
		return [
			{ id: 'open', label: '打开', icon: 'open', action: () => reviewSession(session) },
			{ id: 'rename', label: '重命名', icon: 'edit', action: () => startEdit(session) },
			{ id: 'export', label: '导出', icon: 'export', action: () => downloadSessions([session]) },
			{ id: 'delete', label: '删除', icon: 'delete', danger: true, action: () => (deleteTarget = session) },
		];
	});

	function exportSelected() {
		const selected = sessions.filter((t) => selectedIds.has(t.id));
		downloadSessions(selected);
		cancelSelectMode();
	}

	async function exportHistory() {
		try {
			const json = await invoke('export_history', {
				startDate: startDate || null,
				endDate: endDate || null,
				status: statusFilter || null,
			});
			const blob = new Blob([json], { type: 'application/json' });
			const url = URL.createObjectURL(blob);
			const a = document.createElement('a');
			a.href = url;
			a.download = `haven-history-${new Date().toISOString().slice(0, 10)}.json`;
			a.click();
			URL.revokeObjectURL(url);
		} catch (e) {
			addNotification(`导出失败: ${e}`, 'error', 4000);
		}
	}
</script>

<div class="history-page">
	<div class="header-row">
		<h1>History</h1>
		<span class="count-badge">Total {totalCount} shown</span>
		<div class="header-actions">
			{#if selectMode}
				<button
					class="md-btn md-btn--filled"
					onclick={exportSelected}
					disabled={selectedIds.size === 0}
				>
					Export Selected ({selectedIds.size})
				</button>
				<button class="md-btn md-btn--text" onclick={cancelSelectMode}>Cancel</button>
			{:else}
				<button class="md-btn md-btn--outlined" onclick={enterSelectMode}>Export</button>
				{#if sessions.length > 0}
					<button class="md-btn md-btn--danger" onclick={() => (showClearDialog = true)}>
						Clear All
					</button>
				{/if}
			{/if}
		</div>
	</div>

	<div class="filter-bar">
		<input
			class="md-input"
			type="text"
			placeholder="Search"
			bind:value={searchQuery}
			oninput={handleSearchInput}
			autocomplete="off"
		/>
		<div class="filter-controls">
			<MaterialSelect
				value={statusFilter}
				options={statusOptions}
				onChange={(v) => { statusFilter = v; handleFilterChange(); }}
			/>
			<button class="md-btn md-btn--outlined" onclick={() => (showDateFilter = true)}>
				{#if startDate || endDate}
					Date: {startDate ? startDate.replace(/-/g, '/') : '…'} ~ {endDate ? endDate.replace(/-/g, '/') : '…'}
				{:else}
					Date Filter
				{/if}
			</button>
		</div>
	</div>

	{#if selectMode && sessions.length > 0}
		<div class="select-bar">
			<button class="select-all-row" onclick={toggleSelectAll}>
				<div class="md-checkbox-static" class:checked={selectedIds.size === sessions.length}></div>
				<span>Select all ({sessions.length})</span>
			</button>
		</div>
	{/if}

	{#if sessions.length === 0}
		<div class="empty-state">{loading ? 'Loading...' : 'No session history yet'}</div>
	{:else}
		<div class="history-list">
			{#each sessions as session (session.id)}
			{#if selectMode}
				<button
					class="history-item history-item-btn"
					class:selected={selectedIds.has(session.id)}
					onclick={() => toggleSelect(session.id)}
				>
	<div class="history-item-main">
		<div class="history-top-row">
			<div class="select-checkbox">
				<div class="md-checkbox-static" class:checked={selectedIds.has(session.id)}></div>
			</div>
			<div class="history-title-row">
				<span class="history-title">{displayTitle(session)}</span>
				<MaterialBadge variant={statusVariant(session.status)} text={session.status} />
			</div>
		</div>
		{#if session.transcript}
			<div class="history-message">"{session.transcript}"</div>
		{/if}
		<div class="history-meta">
			<span class="meta-date">{formatMessageTime(session.created_at)}</span>
		</div>
	</div>
				</button>
	{:else}
			<div
				class="history-item"
				class:selected={selectedIds.has(session.id)}
				role="button"
				tabindex="0"
				onclick={() => reviewSession(session)}
				onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), reviewSession(session))}
				oncontextmenu={(e) => openCtxMenu(e, session)}
			>
				<div class="history-item-main">
					<div class="history-title-row">
						{#if editingTitle === session.id}
							<!-- svelte-ignore a11y_autofocus -->
							<input
								type="text"
								class="md-input title-input"
								bind:value={renameValue}
								onkeydown={(e) => handleRenameKeydown(e, session.id)}
								onblur={() => saveTitle(session.id)}
								onclick={(e) => e.stopPropagation()}
								autofocus
								autocomplete="off"
							/>
						{:else}
							<span
								class="history-title"
								onclick={(e) => (e.stopPropagation(), startEdit(session))}
								onkeydown={(e) => e.key === 'Enter' && (e.stopPropagation(), startEdit(session))}
								role="button"
								tabindex="0"
							>
								{displayTitle(session)}
								<svg class="title-edit-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
									<path d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
								</svg>
							</span>
						{/if}
						<MaterialBadge variant={statusVariant(session.status)} text={session.status} />
					</div>
					{#if session.transcript}
						<div class="history-message">"{session.transcript}"</div>
					{/if}
					<div class="history-meta">
						<span class="meta-date">{formatMessageTime(session.created_at)}</span>
						<button
							class="md-btn md-btn--xs md-btn--text delete-btn-meta"
							onclick={(e) => (e.stopPropagation(), deleteTarget = session)}
						>
							Delete
						</button>
					</div>
				</div>
			</div>
		{/if}
			{/each}
		</div>

		{#if hasMore}
			<div class="load-more-row">
				<button class="md-btn md-btn--outlined" onclick={loadMore} disabled={loading}>
					{loading ? 'Loading...' : 'Load More'}
				</button>
			</div>
		{/if}
	{/if}
</div>

<MaterialDialog
	open={showDateFilter}
	onClose={() => (showDateFilter = false)}
	title="Date Filter"
>
	{#snippet children()}
		<div class="date-filter-dialog">
			<div class="date-range-header">
				<span class="date-range-label">Selected range</span>
				<span class="date-range-value">
					{#if startDate || endDate}
						{startDate ? startDate.replace(/-/g, '/') : '…'} — {endDate ? endDate.replace(/-/g, '/') : '…'}
					{:else}
						All dates
					{/if}
				</span>
			</div>
			<div class="date-input-row">
				<div class="date-field">
					<label class="date-filter-label" for="start-date">Start date</label>
					<MaterialDatePicker
						id="start-date"
						value={startDate}
						max={todayISO}
						onChange={(v) => {
							startDate = v;
							if (endDate && endDate < startDate) endDate = '';
							handleFilterChange();
						}}
					/>
				</div>
				<div class="date-field">
					<label class="date-filter-label" for="end-date">End date</label>
					<MaterialDatePicker
						id="end-date"
						value={endDate}
						min={startDate || undefined}
						max={todayISO}
						onChange={(v) => { endDate = v; handleFilterChange(); }}
					/>
				</div>
			</div>
		</div>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => { startDate = ''; endDate = ''; handleFilterChange(); }}>
			Clear
		</button>
		<button class="md-btn md-btn--filled" onclick={() => (showDateFilter = false)}>
			Done
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={deleteTarget !== null}
	onClose={() => (deleteTarget = null)}
	title="Delete Session"
>
	{#snippet children()}
		<p class="dialog-text">
			Delete "{deleteTarget?.input_text || 'Untitled'}"? This action cannot be undone.
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (deleteTarget = null)}>Cancel</button>
		<button class="md-btn md-btn--danger" onclick={() => deleteSession(deleteTarget.id)}>
			Delete
		</button>
	{/snippet}
</MaterialDialog>

<MaterialDialog
	open={showClearDialog}
	onClose={() => (showClearDialog = false)}
	title="Clear All History"
>
	{#snippet children()}
		<p class="dialog-text">
			This will permanently delete all session history. This action cannot be undone.
		</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={() => (showClearDialog = false)}>Cancel</button>
		<button class="md-btn md-btn--danger" onclick={clearHistory}>Clear All</button>
	{/snippet}
</MaterialDialog>

<ContextMenu
	open={ctxMenu.open}
	x={ctxMenu.x}
	y={ctxMenu.y}
	items={ctxMenuItems}
	onClose={closeCtxMenu}
/>

<style>
	.history-page {
		max-width: var(--md-sys-content-max-width);
	}
	.header-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: var(--md-sys-space-xl);
	}
	h1 {
		font-size: 24px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.header-actions {
		display: flex;
		gap: var(--md-sys-space-sm);
	}
	.filter-bar {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-xl);
	}
	.filter-bar > :global(.md-input) {
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
	.date-filter-dialog {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
	}
	.date-range-header {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-medium);
	}
	.date-range-label {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		font-weight: 500;
	}
	.date-range-value {
		font-size: 20px;
		font-weight: 500;
		color: var(--md-sys-color-on-surface);
		line-height: 1.3;
	}
	.date-input-row {
		display: flex;
		gap: var(--md-sys-space-md);
	}
	.date-field {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.date-filter-label {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
		padding-left: var(--md-sys-space-xs);
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
	.empty-state {
		text-align: center;
		padding: 80px 0;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.history-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.history-item {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		transition: box-shadow var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		outline: none;
	}
	.history-item:hover {
		background: var(--md-sys-color-surface-container);
		border-color: var(--md-sys-color-outline);
	}
	.history-item:focus-visible {
		border-color: var(--md-sys-color-primary);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--md-sys-color-primary) 30%, transparent);
	}
	.history-item.selected {
		background: var(--md-sys-color-primary-container);
		border-color: var(--md-sys-color-primary);
	}
	.history-item-main {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		cursor: pointer;
	}
	.history-top-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.history-title-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 1;
		min-width: 0;
	}
	.history-title {
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
	.history-title:hover .title-edit-icon {
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
	.history-message {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container);
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.history-meta {
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
.history-item-btn {
		width: 100%;
		text-align: left;
		font-family: inherit;
		cursor: pointer;
	}
	.history-item-btn:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: -2px;
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
		transition: background-color var(--md-sys-motion-duration-fast)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
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
	.dialog-text {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
	}
	.count-badge {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.load-more-row {
		display: flex;
		justify-content: center;
		padding: var(--md-sys-space-lg) 0;
	}
</style>
