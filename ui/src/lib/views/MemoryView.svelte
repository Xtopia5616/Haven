<script lang="ts">
	import logger from '$lib/logger.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { buildResumeMessages } from '$lib/resumeMessages.ts';
	import { createSessionRefreshScheduler } from '$lib/sessionRefresh.ts';
	import { appSessionReducer, resumeInteractions } from '$lib/sessionReducer.ts';
	import { formatMessageTime } from '$lib/messageFormat.ts';
	import { addNotification } from '$lib/notificationStore.ts';
	import { resumeTargetStore } from '$lib/sessionIntentStore.ts';
	import { clearMediaPlans } from '$lib/mediaPlanStore.ts';
	import { clearToolOutputPreviewsForSession } from '$lib/toolOutputPreviewStore.ts';
	import { isErrorStatus, statusVariant } from '$lib/sessionStatus.ts';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		addFact as addFactCommand,
		deleteFact as deleteFactCommand,
		listFacts,
		recallMemory,
	} from '$lib/memoryCommands.ts';
	import {
		clearHistory as clearHistoryCommand,
		deleteSession as deleteSessionCommand,
		getSessionForResume,
		reopenSession as reopenSessionCommand,
		searchHistoryFiltered,
		updateSessionTitle as updateSessionTitleCommand,
	} from '$lib/sessionHistoryCommands.ts';
	import { registerSessionListener } from '$lib/events.ts';
	import { listActionHistory } from '$lib/actionCommands.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';
	import { openContextMenu } from '$lib/contextMenu.ts';
	import SessionHistory from './SessionHistory.svelte';
	import MemoryCenter from './MemoryCenter.svelte';
	import TaskCenter from '$lib/TaskCenter.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceSectionHeader from '$lib/WorkspaceSectionHeader.svelte';
	import type { HistoryFilterRequest } from '$lib/contracts/commands.ts';
	import type { Fact, MemoryRecallState } from '$lib/contracts/memory.ts';
	import type { ActionKind, ActionPayload } from '$lib/contracts/action.ts';
	import type { SessionHistoryRow } from '$lib/contracts/sessionHistory.ts';
	import type { ContextMenuItem } from '$lib/contextMenu.ts';

	type MemorySession = SessionHistoryRow;
	type MemoryTabId = 'sessions' | 'tasks' | 'memory';
	type TaskAction = ActionPayload;

	interface Props {
		onNewSession?: () => void;
		runningBackgroundActions?: ActionPayload[];
		pendingScheduledActions?: ActionPayload[];
		actionStatusLabel?: (status: string) => string;
		sessionTitleFor?: (action: Pick<ActionPayload, 'sessionId'>) => string;
		actionDuration?: (action: ActionPayload) => string;
		scheduledActionCountdown?: (dueAt?: string) => string;
		onOpenSession?: (sessionId: string) => void;
		onCancel?: (actionId: string, kind?: ActionKind) => void;
	}

	let {
		onNewSession = () => {},
		runningBackgroundActions = [],
		pendingScheduledActions = [],
		actionStatusLabel = (status) => status || '',
		sessionTitleFor = () => '',
		actionDuration = () => '',
		scheduledActionCountdown = () => '',
		onOpenSession = () => {},
		onCancel = () => {},
	}: Props = $props();

	let sessions = $state<MemorySession[]>([]);
	let searchQuery = $state('');
	let searchTimer: ReturnType<typeof setTimeout> | null = null;
	let deleteTarget = $state<MemorySession | null>(null);
	let showClearDialog = $state(false);
	let selectMode = $state(false);
	let selectedIds = $state(new Set<string>());
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	let loadSessionsSeq = 0;
	let loadFactsSeq = 0;
	let loadTaskHistorySeq = 0;
	const PAGE_SIZE = 50;
	let statusFilter = $state('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);
	let editingTitle = $state<string | null>(null);
	let renameValue = $state('');
	const MEMORY_TAB_IDS: readonly MemoryTabId[] = ['sessions', 'tasks', 'memory'];
	function isMemoryTabId(value: string | null): value is MemoryTabId {
		return value !== null && MEMORY_TAB_IDS.includes(value as MemoryTabId);
	}
	function memoryTabFromUrl(): MemoryTabId {
		const section = get(page).url.searchParams.get('section');
		return isMemoryTabId(section) ? section : 'sessions';
	}
	let activeTab = $state<MemoryTabId>(memoryTabFromUrl());
	const memoryTabs = [
		{ id: 'sessions', label: '会话历史' },
		{ id: 'tasks', label: '任务历史' },
		{ id: 'memory', label: '长期记忆' },
	];
	let taskHistory = $state<TaskAction[]>([]);
	let taskHistoryLoading = $state(false);
	let taskHistoryFailed = $state(false);
	let memoryRecall = $state<MemoryRecallState>({
		query: '',
		kind: 'all',
		results: [],
		loading: false,
		searched: false,
	});
	let facts = $state<Fact[]>([]);
	let factsLoaded = $state(false);
	/** @type {'' | 'user' | 'inferred'} */
	let factSourceFilter = $state<'' | 'user' | 'inferred'>('');
	let newFact = $state<{ predicate: string; object: string; tags: string }>({
		predicate: '',
		object: '',
		tags: '',
	});
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
	let unlistenTitleUpdate: { dispose: () => void } | null = null;
	let unlistenLifecycle: Array<{ dispose: () => void }> = [];
	const sessionsRefresh = createSessionRefreshScheduler(() => loadSessionsNow());

	onMount(async () => {
		await sessionsRefresh.refresh();
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
			sessionsRefresh.schedule();
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
		sessionsRefresh.dispose();
		unlistenTitleUpdate?.dispose();
		unlistenLifecycle.forEach((registration) => registration.dispose());
		unlistenLifecycle = [];
	});
	$effect(() => {
		const section = $page.url.searchParams.get('section');
		const nextTab = isMemoryTabId(section) ? section : 'sessions';
		if (activeTab !== nextTab) activeTab = nextTab;
	});
	$effect(() => {
		if (activeTab !== 'memory') return;
		factSourceFilter;
		loadFacts();
	});
	$effect(() => {
		if (activeTab !== 'tasks') return;
		loadTaskHistory();
	});

	async function loadTaskHistory() {
		const sequence = ++loadTaskHistorySeq;
		taskHistoryLoading = true;
		taskHistoryFailed = false;
		try {
			const rows = await listActionHistory(undefined, 100);
			if (sequence !== loadTaskHistorySeq) return;
			taskHistory = rows;
		} catch (e) {
			if (sequence !== loadTaskHistorySeq) return;
			taskHistory = [];
			taskHistoryFailed = true;
			reportError(e, { context: 'MemoryView', message: '加载任务历史失败', log: false });
		} finally {
			if (sequence === loadTaskHistorySeq) taskHistoryLoading = false;
		}
	}

	function selectMemoryTab(tabId: string) {
		if (!isMemoryTabId(tabId)) return;
		activeTab = tabId;
		const params = new URLSearchParams(get(page).url.searchParams);
		params.set('tab', 'memory');
		if (tabId === 'sessions') params.delete('section');
		else params.set('section', tabId);
		void goto('/?' + params.toString(), { replaceState: true });
	}

	function filterParams(extra: Pick<HistoryFilterRequest, 'limit' | 'offset'>) {
		return {
			query: searchQuery || null,
			status: statusFilter || null,
			startDate: startDate || null,
			endDate: endDate || null,
			...extra,
		};
	}
	function setSearchQuery(value: string) {
		searchQuery = value;
	}
	function requestDelete(session: MemorySession) {
		deleteTarget = session;
	}
	async function loadSessionsNow() {
		const sequence = ++loadSessionsSeq;
		loading = true;
		try {
			const results = await searchHistoryFiltered(
				filterParams({ limit: PAGE_SIZE, offset: 0 }),
			);
			if (sequence !== loadSessionsSeq) return;
			sessions = results || [];
			totalCount = sessions.length;
			offset = PAGE_SIZE;
			hasMore = sessions.length >= PAGE_SIZE;
		} catch (error) {
			if (sequence !== loadSessionsSeq) return;
			sessions = [];
			totalCount = 0;
			hasMore = false;
			reportError(error, {
				context: 'MemoryView',
				message: '加载会话列表失败',
				log: false,
			});
		}
		if (sequence === loadSessionsSeq) loading = false;
	}
	async function loadMore() {
		if (loading || !hasMore) return;
		const sequence = loadSessionsSeq;
		loading = true;
		try {
			const more = await searchHistoryFiltered(filterParams({ limit: PAGE_SIZE, offset }));
			if (sequence !== loadSessionsSeq) return;
			if (more && more.length > 0) {
				sessions = [...sessions, ...more];
				offset += more.length;
				hasMore = more.length >= PAGE_SIZE;
				totalCount = sessions.length;
			} else hasMore = false;
		} catch (error) {
			if (sequence !== loadSessionsSeq) return;
			hasMore = false;
			reportError(error, {
				context: 'MemoryView',
				message: '加载更多会话失败',
				log: false,
			});
		}
		if (sequence === loadSessionsSeq) loading = false;
	}
	function handleSearchInput() {
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			searchTimer = null;
			void sessionsRefresh.refresh();
		}, 300);
	}
	function handleFilterChange() {
		void sessionsRefresh.refresh();
	}
	function clearHistoryFilters() {
		searchQuery = '';
		statusFilter = '';
		startDate = '';
		endDate = '';
		void sessionsRefresh.refresh();
	}
	function handleStatusFilterChange(value: string) {
		statusFilter = value;
		handleFilterChange();
	}
	function handleRecallKindChange(value: string) {
		memoryRecall.kind = value;
		memoryRecall.results = [];
		memoryRecall.searched = false;
	}
	function handleStartDateChange(value: string) {
		startDate = value;
		if (endDate && endDate < startDate) endDate = '';
		handleFilterChange();
	}
	function handleEndDateChange(value: string) {
		endDate = value;
		handleFilterChange();
	}
	async function resumeSession(session: MemorySession) {
		try {
			const wasError = isErrorStatus(session.status);
			// Opening an errored conversation is read-only. Reopening it here used
			// to change the in-memory status to Paused before the chat could render,
			// which hid the actual failure state. Continue/retry performs the
			// explicit transition when the user asks for it.
			if (!wasError) await reopenSessionCommand({ sessionId: session.id });
			const result = await getSessionForResume({ sessionId: session.id });
			appSessionReducer.dispatch({
				type: 'session/messages/resume-loaded',
				sessionId: session.id,
				messages: buildResumeMessages(result),
				interactions: resumeInteractions(result),
				usage: result.usage,
				llmUsage: result.llm_usage,
			});
			resumeTargetStore.set({
				sessionId: session.id,
				summary: session.input_text,
				title: session.title,
				status: result.session?.status || session.status,
				wasError,
				errorReason: wasError ? appSessionReducer.getSessionErrorReason(session.id) : '',
			});
			await goto('/');
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '加载会话详情失败', log: false });
		}
	}
	async function deleteSession(sessionId: string) {
		try {
			await deleteSessionCommand({ sessionId });
			sessions = sessions.filter((session) => session.id !== sessionId);
			totalCount = sessions.length;
			clearMediaPlans(sessionId);
			clearToolOutputPreviewsForSession(sessionId);
			appSessionReducer.dispatch({ type: 'session/deleted', sessionId });
			addNotification('会话已删除', 'success', 2000);
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '删除失败', log: false });
		}
		deleteTarget = null;
	}
	async function clearSessions() {
		try {
			const count = await clearHistoryCommand();
			sessions = [];
			totalCount = 0;
			hasMore = false;
			clearMediaPlans(null);
			clearToolOutputPreviewsForSession(null);
			appSessionReducer.dispatch({ type: 'sessions/cleared' });
			addNotification(`已清空 ${count} 条会话`, 'success', 3000);
		} catch (error) {
			reportError(error, {
				context: 'MemoryView',
				message: '清空会话失败',
				log: false,
			});
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
	function toggleSelect(sessionId: string) {
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
	function displayTitle(session: MemorySession) {
		if (session.title) return session.title;
		const text = session.input_text || '';
		const match = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		return (match ? match[0].trim() : text.trim()) || '未命名会话';
	}
	function startEdit(session: MemorySession) {
		editingTitle = session.id;
		const text = session.input_text || '';
		const match = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		renameValue = session.title || (match ? match[0].trim() : text.trim());
	}
	function cancelEdit() {
		editingTitle = null;
		renameValue = '';
	}
	async function saveTitle(sessionId: string) {
		const value = renameValue.trim();
		if (!value) {
			cancelEdit();
			return;
		}
		try {
			await updateSessionTitleCommand({ sessionId, title: value });
			const session = sessions.find((item) => item.id === sessionId);
			if (session) session.title = value;
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '重命名失败', log: false });
		}
		cancelEdit();
	}
	function handleRenameKeydown(event: KeyboardEvent, sessionId: string) {
		if (event.key === 'Enter') {
			event.preventDefault();
			saveTitle(sessionId);
		} else if (event.key === 'Escape') cancelEdit();
	}
	function handleRenameValueChange(value: string) {
		renameValue = value;
	}
	function downloadSessions(sessionsToExport: MemorySession[]) {
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
	function openCtxMenu(event: MouseEvent, session: MemorySession) {
		openContextMenu(event, buildContextMenuItems(session));
	}
	function buildContextMenuItems(session: MemorySession): ContextMenuItem[] {
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
				action: () => {
					deleteTarget = session;
				},
			},
		];
	}
	function exportSelected() {
		downloadSessions(sessions.filter((session) => selectedIds.has(session.id)));
		cancelSelectMode();
	}

	async function loadFacts() {
		const sequence = ++loadFactsSeq;
		try {
			const rows = (await listFacts({ source: factSourceFilter || null })) || [];
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
	function handleFactSourceFilterChange(value: string) {
		if (value === '' || value === 'user' || value === 'inferred') factSourceFilter = value;
	}
	async function addFact() {
		const predicate = newFact.predicate.trim();
		const object = newFact.object.trim();
		if (!predicate || !object) {
			addNotification('请输入谓词和对象', 'error', 3000);
			return false;
		}
		addingFact = true;
		try {
			const tags = newFact.tags
				.split(',')
				.map((tag) => tag.trim())
				.filter(Boolean);
			await addFactCommand({
				subject: 'user',
				predicate,
				object,
				tags: tags.length ? tags : null,
			});
			newFact = { predicate: '', object: '', tags: '' };
			await loadFacts();
			addNotification('事实已保存', 'success', 2500);
			return true;
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '添加事实失败', log: false });
			return false;
		} finally {
			addingFact = false;
		}
	}
	async function deleteFact(factId: string) {
		try {
			await deleteFactCommand({ factId });
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
			const kinds: Array<'fact' | 'episode'> =
				memoryRecall.kind === 'all'
					? ['fact', 'episode']
					: [memoryRecall.kind as 'fact' | 'episode'];
			const limit = memoryRecall.kind === 'all' ? 5 : 10;
			const resultGroups = await Promise.all(
				kinds.map(async (kind) => {
					const results = (await recallMemory({ query, kind, limit })) || [];
					return results.map((result) => ({ ...result, kind }));
				}),
			);
			memoryRecall.results = resultGroups.flat();
			memoryRecall.searched = true;
		} catch (e) {
			memoryRecall.results = [];
			memoryRecall.searched = true;
			reportError(e, { context: 'MemoryView', message: '记忆检索失败', log: false });
		} finally {
			memoryRecall.loading = false;
		}
	}
	function clearMemoryRecall() {
		memoryRecall.query = '';
		memoryRecall.results = [];
		memoryRecall.searched = false;
	}
</script>

<div class="memory-page">
	<WorkspacePageHeader title="历史" description="回顾会话、任务和长期记忆。" />
	<MaterialTabs
		tabs={memoryTabs}
		activeTab={activeTab ?? 'sessions'}
		onNavigate={selectMemoryTab}
		ariaLabel="历史分区"
		idPrefix="memory-tab"
		panelId="memory-panel"
		className="workspace-secondary-tabs"
	/>
	{#key activeTab}
		<div
			id="memory-panel"
			class="memory-panel motion-surface-enter"
			role="tabpanel"
			aria-label={memoryTabs.find((tab) => tab.id === activeTab)?.label || '历史'}
		>
			{#if activeTab === 'sessions'}
				<WorkspaceSectionHeader title="会话历史" description="查看并继续过去的对话。" />
				<SessionHistory
					{sessions}
					{searchQuery}
					{totalCount}
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
					onClearFilters={clearHistoryFilters}
					onStatusFilterChange={handleStatusFilterChange}
					onOpenDateFilter={() => {
						showDateFilter = true;
					}}
					onToggleSelectAll={toggleSelectAll}
					onToggleSelect={toggleSelect}
					onEnterSelectMode={enterSelectMode}
					onCancelSelectMode={cancelSelectMode}
					onExportSelected={exportSelected}
					onOpenClearDialog={() => (showClearDialog = true)}
					onResume={resumeSession}
					{onNewSession}
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
			{:else if activeTab === 'tasks'}
				<WorkspaceSectionHeader
					title="任务历史"
					description="查看后台任务和定时任务的当前状态及最近历史。"
				/>
				<TaskCenter
					{runningBackgroundActions}
					{pendingScheduledActions}
					{taskHistory}
					{taskHistoryLoading}
					{taskHistoryFailed}
					onRefreshTaskHistory={loadTaskHistory}
					{actionStatusLabel}
					{sessionTitleFor}
					{actionDuration}
					{scheduledActionCountdown}
					{onOpenSession}
					{onCancel}
				/>
			{:else}
				<div class="memory-tools-view" aria-label="长期记忆">
					<WorkspaceSectionHeader
						title="长期记忆"
						description="管理已保存的长期事实，或检索过去的对话。"
					/>
					<MemoryCenter
						{facts}
						{factsLoaded}
						{factSourceFilter}
						{factSourceOptions}
						{newFact}
						{addingFact}
						{memoryRecall}
						onRecallKindChange={handleRecallKindChange}
						onRunRecall={runRecall}
						onClearRecall={clearMemoryRecall}
						onFactSourceFilterChange={handleFactSourceFilterChange}
						onAddFact={addFact}
						onDeleteFact={deleteFact}
					/>
				</div>
			{/if}
		</div>
	{/key}
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

<style>
	.memory-page {
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
	}
	.memory-panel {
		min-width: 0;
	}
	.memory-tools-view {
		display: flex;
		flex-direction: column;
		gap: 0;
		min-width: 0;
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
	@media (orientation: landscape) and (min-width: 1280px) {
		.memory-page {
			max-width: none;
		}
		.memory-panel,
		.memory-tools-view {
			width: 100%;
		}
	}
	@media (max-width: 700px) {
		.date-input-row {
			flex-direction: column;
		}
	}
</style>
