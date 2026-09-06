<script>
	/** @typedef {{ id: string; title?: string; input_text?: string; transcript?: string; status: string; created_at: string; [key: string]: any }} MemorySession */
	import logger from '$lib/logger.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { buildResumeMessages, mergeLiveStreaming } from '$lib/resumeMessages.ts';
	import {
		formatMessageTime,
		addNotification,
		activeSessionIdStore,
		resumeTargetStore,
	} from '$lib/stores.ts';
	import {
		clearAllSessionMessages,
		clearSessionMessages,
		updateSessionMessages,
	} from '$lib/sessionMessages.ts';
	import { restoreSessionLlmUsage, restoreSessionTokenStats } from '$lib/sessionUsage.ts';
	import { statusVariant } from '$lib/sessionStatus.ts';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { invoke } from '$lib/tauri.ts';
	import { registerSessionListener } from '$lib/events.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import SessionHistory from './SessionHistory.svelte';
	import LongTermFacts from './LongTermFacts.svelte';
	import MemoryRecall from './MemoryRecall.svelte';

	/** @type {MemorySession[]} */
	let sessions = $state([]);
	let searchQuery = $state('');
	/** @type {ReturnType<typeof setTimeout> | null} */
	let searchTimer = null;
	let deleteTarget = /** @type {Record<string, any> | null} */ ($state(null));
	let showClearDialog = $state(false);
	let selectMode = $state(false);
	let selectedIds = $state(new Set());
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	let loadSessionsSeq = 0;
	let loadFactsSeq = 0;
	const PAGE_SIZE = 50;
	let statusFilter = $state('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);
	/** @type {string | null} */
	let editingTitle = $state(null);
	let renameValue = $state('');
	/** @type {{ open: boolean; x: number; y: number; session: MemorySession | null }} */
	let ctxMenu = $state({ open: false, x: 0, y: 0, session: null });
	let activeTab = $state('sessions');
	const memoryTabs = [
		{ id: 'sessions', label: '会话' },
		{ id: 'facts', label: '事实' },
		{ id: 'recall', label: '检索' },
	];
	let memoryRecall = $state({
		query: '',
		kind: 'fact',
		results: [],
		loading: false,
		searched: false,
	});
	/** @type {any[]} */
	let facts = $state([]);
	let factsLoaded = $state(false);
	/** @type {'' | 'user' | 'inferred'} */
	let factSourceFilter = $state('');
	let newFact = $state({ predicate: '', object: '', tags: '' });
	let addingFact = $state(false);
	const statusOptions = [
		{ value: '', label: '全部状态' },
		{ value: 'completed', label: '已完成' },
		{ value: 'paused', label: '已暂停' },
		{ value: 'error', label: '错误' },
	];
	const factSourceOptions = [
		{ value: '', label: '全部来源' },
		{ value: 'user', label: '手动' },
		{ value: 'inferred', label: '推断' },
	];
	const todayISO = $derived.by(() => {
		const now = new Date();
		return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
	});
	/** @type {{ dispose: () => void } | null} */
	let unlistenTitleUpdate = null;
	/** @type {Array<{ dispose: () => void }>} */
	let unlistenLifecycle = [];
	/** @type {ReturnType<typeof setTimeout> | null} */
	let reloadTimer = null;

	onMount(async () => {
		await loadSessions();
		unlistenTitleUpdate = await registerSessionListener(
			'session:title-updated',
			(event) => {
				const { sessionId, title } = event.payload;
				sessions = sessions.map((session) =>
					session.id === sessionId ? { ...session, title } : session,
				);
			},
			{ tag: 'memory' },
		);
		const scheduleReload = () => {
			if (reloadTimer) clearTimeout(reloadTimer);
			reloadTimer = setTimeout(loadSessions, 300);
		};
		unlistenLifecycle = await Promise.all([
			registerSessionListener('session:created', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:updated', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:completed', scheduleReload, { tag: 'memory' }),
			registerSessionListener('session:error', scheduleReload, { tag: 'memory' }),
		]);
	});
	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
		if (reloadTimer) clearTimeout(reloadTimer);
		unlistenTitleUpdate?.dispose();
		unlistenLifecycle.forEach((registration) => registration.dispose());
		unlistenLifecycle = [];
	});
	$effect(() => {
		if (activeTab !== 'facts') return;
		factSourceFilter;
		loadFacts();
	});

	/** @param {Record<string, any>} extra */
	function filterParams(extra) {
		return {
			query: searchQuery || null,
			status: statusFilter || null,
			startDate: startDate || null,
			endDate: endDate || null,
			...extra,
		};
	}
	/** @param {string} value */
	function setSearchQuery(value) {
		searchQuery = value;
	}
	/** @param {MemorySession} session */
	function requestDelete(session) {
		deleteTarget = session;
	}
	async function loadSessions() {
		const sequence = ++loadSessionsSeq;
		loading = true;
		try {
			const results = await invoke(
				'search_history_filtered',
				filterParams({ limit: PAGE_SIZE, offset: 0 }),
			);
			if (sequence !== loadSessionsSeq) return;
			sessions = results || [];
			totalCount = sessions.length;
			offset = PAGE_SIZE;
			hasMore = sessions.length >= PAGE_SIZE;
		} catch {
			if (sequence !== loadSessionsSeq) return;
			sessions = [];
			totalCount = 0;
			hasMore = false;
			addNotification('加载会话列表失败', 'error', 3000);
		}
		if (sequence === loadSessionsSeq) loading = false;
	}
	async function loadMore() {
		if (loading || !hasMore) return;
		const sequence = loadSessionsSeq;
		loading = true;
		try {
			const more = await invoke(
				'search_history_filtered',
				filterParams({ limit: PAGE_SIZE, offset }),
			);
			if (sequence !== loadSessionsSeq) return;
			if (more && more.length > 0) {
				sessions = [...sessions, ...more];
				offset += more.length;
				hasMore = more.length >= PAGE_SIZE;
				totalCount = sessions.length;
			} else hasMore = false;
		} catch {
			if (sequence !== loadSessionsSeq) return;
			hasMore = false;
			addNotification('加载更多会话失败', 'error', 3000);
		}
		if (sequence === loadSessionsSeq) loading = false;
	}
	function handleSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(loadSessions, 300);
	}
	function handleFilterChange() {
		loadSessions();
	}
	/** @param {string} value */
	function handleStatusFilterChange(value) {
		statusFilter = value;
		handleFilterChange();
	}
	/** @param {string} value */
	function handleRecallKindChange(value) {
		memoryRecall.kind = value;
	}
	/** @param {string} value */
	function handleStartDateChange(value) {
		startDate = value;
		if (endDate && endDate < startDate) endDate = '';
		handleFilterChange();
	}
	/** @param {string} value */
	function handleEndDateChange(value) {
		endDate = value;
		handleFilterChange();
	}
	/** @param {MemorySession} session */
	async function resumeSession(session) {
		try {
			await invoke('reopen_session', { sessionId: session.id });
			const result = await invoke('get_session_for_resume', { sessionId: session.id });
			updateSessionMessages(session.id, (existing) =>
				mergeLiveStreaming(buildResumeMessages(result), existing),
			);
			restoreSessionTokenStats(session.id, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(session.id, result.llm_usage);
			resumeTargetStore.set({
				sessionId: session.id,
				summary: session.input_text,
				title: session.title,
				wasError: session.status === 'error' || session.status === 'failed',
			});
			await goto('/');
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '加载会话详情失败', log: false });
		}
	}
	/** @param {string} sessionId */
	async function deleteSession(sessionId) {
		try {
			await invoke('delete_session', { sessionId });
			sessions = sessions.filter((session) => session.id !== sessionId);
			totalCount = sessions.length;
			if (get(activeSessionIdStore) === sessionId) {
				activeSessionIdStore.set(null);
				clearSessionMessages(sessionId);
			}
			addNotification('会话已删除', 'success', 2000);
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '删除失败', log: false });
		}
		deleteTarget = null;
	}
	async function clearSessions() {
		try {
			const count = await invoke('clear_history');
			sessions = [];
			totalCount = 0;
			hasMore = false;
			activeSessionIdStore.set(null);
			clearAllSessionMessages();
			addNotification(`已清空 ${count} 条会话`, 'success', 3000);
		} catch {
			addNotification('清空会话失败', 'error', 4000);
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
	/** @param {string} sessionId */
	function toggleSelect(sessionId) {
		const next = new Set(selectedIds);
		if (next.has(sessionId)) next.delete(sessionId);
		else next.add(sessionId);
		selectedIds = next;
	}
	function toggleSelectAll() {
		selectedIds =
			selectedIds.size === sessions.length
				? new Set()
				: new Set(sessions.map((session) => session.id));
	}
	/** @param {MemorySession} session */
	function displayTitle(session) {
		if (session.title) return session.title;
		const text = session.input_text || '';
		const match = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		return (match ? match[0].trim() : text.trim()) || '未命名会话';
	}
	/** @param {MemorySession} session */
	function startEdit(session) {
		editingTitle = session.id;
		const text = session.input_text || '';
		const match = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		renameValue = session.title || (match ? match[0].trim() : text.trim());
	}
	function cancelEdit() {
		editingTitle = null;
		renameValue = '';
	}
	/** @param {string} sessionId */
	async function saveTitle(sessionId) {
		const value = renameValue.trim();
		if (!value) {
			cancelEdit();
			return;
		}
		try {
			await invoke('update_session_title', { sessionId, title: value });
			const session = sessions.find((item) => item.id === sessionId);
			if (session) session.title = value;
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '重命名失败', log: false });
		}
		cancelEdit();
	}
	/** @param {KeyboardEvent} event @param {string} sessionId */
	function handleRenameKeydown(event, sessionId) {
		if (event.key === 'Enter') {
			event.preventDefault();
			saveTitle(sessionId);
		} else if (event.key === 'Escape') cancelEdit();
	}
	/** @param {string} value */
	function handleRenameValueChange(value) {
		renameValue = value;
	}
	/** @param {MemorySession[]} sessionsToExport */
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
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = `haven-sessions-${new Date().toISOString().slice(0, 10)}.json`;
		anchor.click();
		URL.revokeObjectURL(url);
	}
	/** @param {MouseEvent} event @param {MemorySession} session */
	function openCtxMenu(event, session) {
		event.preventDefault();
		event.stopPropagation();
		ctxMenu = { open: true, x: event.clientX, y: event.clientY, session };
	}
	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, session: null };
	}
	let ctxMenuItems = $derived.by(() => {
		const session = ctxMenu.session;
		if (!session) return [];
		return [
			{ id: 'open', label: '打开', icon: 'open', action: () => resumeSession(session) },
			{ id: 'rename', label: '重命名', icon: 'edit', action: () => startEdit(session) },
			{
				id: 'export',
				label: '导出',
				icon: 'export',
				action: () => downloadSessions([session]),
			},
			{
				id: 'delete',
				label: '删除',
				icon: 'delete',
				danger: true,
				action: () => (deleteTarget = session),
			},
		];
	});
	function exportSelected() {
		downloadSessions(sessions.filter((session) => selectedIds.has(session.id)));
		cancelSelectMode();
	}

	async function loadFacts() {
		const sequence = ++loadFactsSeq;
		try {
			const rows = (await invoke('list_facts', { source: factSourceFilter || null })) || [];
			if (sequence !== loadFactsSeq) return;
			facts = rows;
			factsLoaded = true;
		} catch {
			if (sequence !== loadFactsSeq) return;
			facts = [];
			factsLoaded = true;
			logger.warn('memory', 'load facts error');
		}
	}
	/** @param {string} value */
	function handleFactSourceFilterChange(value) {
		factSourceFilter = /** @type {'' | 'user' | 'inferred'} */ (value);
	}
	async function addFact() {
		const predicate = newFact.predicate.trim();
		const object = newFact.object.trim();
		if (!predicate || !object) {
			addNotification('请输入 predicate 和 object', 'error', 3000);
			return;
		}
		addingFact = true;
		try {
			const tags = newFact.tags
				.split(',')
				.map((tag) => tag.trim())
				.filter(Boolean);
			await invoke('add_fact', {
				subject: 'user',
				predicate,
				object,
				tags: tags.length ? tags : null,
			});
			newFact = { predicate: '', object: '', tags: '' };
			await loadFacts();
			addNotification('事实已保存', 'success', 2500);
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '添加事实失败', log: false });
		} finally {
			addingFact = false;
		}
	}
	/** @param {string} factId */
	async function deleteFact(factId) {
		try {
			await invoke('delete_fact', { factId });
			facts = facts.filter((fact) => fact.id !== factId);
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '删除事实失败', log: false });
		}
	}
	async function runRecall() {
		const query = memoryRecall.query.trim();
		if (!query) return;
		memoryRecall.loading = true;
		try {
			memoryRecall.results =
				(await invoke('recall_memory', { query, kind: memoryRecall.kind, limit: 10 })) ||
				[];
			memoryRecall.searched = true;
		} catch (e) {
			memoryRecall.results = [];
			memoryRecall.searched = true;
			reportError(e, { context: 'MemoryView', message: '记忆检索失败', log: false });
		} finally {
			memoryRecall.loading = false;
		}
	}
</script>

<div class="memory-page">
	<div class="header-row">
		<h1>记忆</h1>
		{#if activeTab === 'sessions'}<span class="count-badge">已显示 {totalCount} 条</span>
			<div class="header-actions">
				{#if selectMode}
					<MaterialButton
						variant="filled"
						label={`导出选中（${selectedIds.size}）`}
						onclick={exportSelected}
						disabled={selectedIds.size === 0}
					/>
					<MaterialButton variant="text" label="取消" onclick={cancelSelectMode} />
				{:else}
					<MaterialButton
						variant="outlined"
						className="memory-header-action"
						label="导出"
						onclick={enterSelectMode}
					/>
					{#if sessions.length > 0}
						<MaterialButton
							variant="danger"
							className="memory-header-action"
							label="清空会话"
							onclick={() => (showClearDialog = true)}
						/>
					{/if}
				{/if}
			</div>{/if}
	</div>
	<div class="md-tabs memory-tabs" role="tablist">
		{#each memoryTabs as tab}<button
				class="md-tab"
				class:active={activeTab === tab.id}
				role="tab"
				aria-selected={activeTab === tab.id}
				onclick={() => (activeTab = tab.id)}>{tab.label}</button
			>{/each}
	</div>
	{#if activeTab === 'sessions'}
		<SessionHistory
			{sessions}
			{searchQuery}
			{statusFilter}
			{statusOptions}
			{startDate}
			{endDate}
			{selectMode}
			{selectedIds}
			{loading}
			{hasMore}
			{editingTitle}
			{renameValue}
			onSearchQueryChange={setSearchQuery}
			onSearchInput={handleSearchInput}
			onStatusFilterChange={handleStatusFilterChange}
			onOpenDateFilter={() => {
				showDateFilter = true;
			}}
			onToggleSelectAll={toggleSelectAll}
			onToggleSelect={toggleSelect}
			onResume={resumeSession}
			onStartEdit={startEdit}
			onRenameValueChange={handleRenameValueChange}
			onRenameKeydown={handleRenameKeydown}
			onSaveTitle={saveTitle}
			onContextMenu={openCtxMenu}
			onDeleteRequest={requestDelete}
			onLoadMore={loadMore}
			{displayTitle}
			{statusVariant}
			{formatMessageTime}
		/>
	{:else if activeTab === 'facts'}
		<LongTermFacts
			{facts}
			{factsLoaded}
			{factSourceFilter}
			{factSourceOptions}
			{newFact}
			{addingFact}
			onFactSourceFilterChange={handleFactSourceFilterChange}
			onAddFact={addFact}
			onDeleteFact={deleteFact}
		/>
	{:else}
		<MemoryRecall
			{memoryRecall}
			onRecallKindChange={handleRecallKindChange}
			onRunRecall={runRecall}
		/>
	{/if}
</div>

<MaterialDialog open={showDateFilter} onClose={() => (showDateFilter = false)} title="日期筛选">
	{#snippet children()}
		<div class="date-filter-dialog">
			<div class="date-range-header">
				<span class="date-range-label">已选范围</span><span class="date-range-value"
					>{#if startDate || endDate}{startDate ? startDate.replace(/-/g, '/') : '…'} — {endDate
							? endDate.replace(/-/g, '/')
							: '…'}{:else}全部日期{/if}</span
				>
			</div>
			<div class="date-input-row">
				<div class="date-field">
					<label class="date-filter-label" for="start-date">开始日期</label
					><MaterialDatePicker
						id="start-date"
						value={startDate}
						max={todayISO}
						onChange={handleStartDateChange}
					/>
				</div>
				<div class="date-field">
					<label class="date-filter-label" for="end-date">结束日期</label
					><MaterialDatePicker
						id="end-date"
						value={endDate}
						min={startDate || undefined}
						max={todayISO}
						onChange={handleEndDateChange}
					/>
				</div>
			</div>
		</div>
	{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="清除"
			onclick={() => {
				startDate = '';
				endDate = '';
				handleFilterChange();
			}}
		/>
		<MaterialButton variant="filled" label="完成" onclick={() => (showDateFilter = false)} />
	{/snippet}
</MaterialDialog>
<MaterialDialog open={deleteTarget !== null} onClose={() => (deleteTarget = null)} title="删除会话">
	{#snippet children()}<p class="dialog-text">
			确定删除「{deleteTarget?.title ||
				deleteTarget?.input_text ||
				'未命名会话'}」？此操作不可撤销。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton variant="text" label="取消" onclick={() => (deleteTarget = null)} />
		<MaterialButton
			variant="danger"
			label="删除"
			onclick={() => {
				if (deleteTarget) deleteSession(deleteTarget.id);
			}}
		/>
	{/snippet}
</MaterialDialog>
<MaterialDialog open={showClearDialog} onClose={() => (showClearDialog = false)} title="清空会话">
	{#snippet children()}<p class="dialog-text">
			将永久删除全部会话记录（长期事实不受影响）。此操作不可撤销。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton variant="text" label="取消" onclick={() => (showClearDialog = false)} />
		<MaterialButton variant="danger" label="清空全部" onclick={clearSessions} />
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
	.memory-page {
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
	}
	.header-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-xl);
	}
	h1 {
		font-family: var(--md-ref-typeface-brand);
		font-size: var(--md-sys-typescale-headline-large-size);
		font-weight: 700;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-headline-large-line-height);
		color: var(--md-sys-color-on-surface);
		margin: 0;
	}
	.header-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
		margin-left: auto;
	}
	.header-actions :global(.memory-header-action) {
		flex: 0 0 112px;
		width: 112px;
	}
	.count-badge {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
		margin-right: var(--md-sys-space-md);
	}
	.memory-tabs {
		margin-bottom: var(--md-sys-space-xl);
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
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		font-weight: 500;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.date-range-value {
		font-size: var(--md-sys-typescale-headline-medium-size);
		font-weight: 500;
		color: var(--md-sys-color-on-surface);
		line-height: var(--md-sys-typescale-headline-medium-line-height);
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
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding-left: var(--md-sys-space-xs);
	}
	.dialog-text {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	@media (max-width: 700px) {
		.header-row {
			align-items: flex-start;
			gap: var(--md-sys-space-sm);
		}
		.count-badge {
			margin-left: 0;
		}
		.header-actions {
			width: 100%;
			margin-left: 0;
		}
		.header-actions :global(.md-btn) {
			flex: 1 1 0;
			width: 0;
			min-width: 0;
		}
		.date-input-row {
			flex-direction: column;
		}
	}
</style>
