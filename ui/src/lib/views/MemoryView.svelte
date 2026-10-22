<script lang="ts">
	import { reportError } from '$lib/errorHandling.ts';
	import { buildResumeMessages } from '$lib/resumeMessages.ts';
	import { createSessionRefreshScheduler } from '$lib/sessionRefresh.ts';
	import { appSessionReducer, type SessionRunEndStatus } from '$lib/sessionReducer.ts';
	import { formatMessageTime } from '$lib/messageFormat.ts';
	import { addNotification } from '$lib/notificationStore.ts';
	import { sessionResumeTargetStore } from '$lib/sessionIntentStore.ts';
	import { clearMediaPlans } from '$lib/mediaPlanStore.ts';
	import { clearToolOutputPreviewsForSession } from '$lib/toolOutputPreviewStore.ts';
	import { isErrorStatus, statusVariant } from '$lib/sessionStatus.ts';
	import { onMount, onDestroy, type ComponentProps } from 'svelte';
	import { get } from 'svelte/store';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		addFact as addFactCommand,
		clearFacts as clearFactsCommand,
		deleteFact as deleteFactCommand,
		listFacts,
		listFactProposals,
		reviewFactProposal as reviewFactProposalCommand,
		recallMemory,
	} from '$lib/memoryCommands.ts';
	import {
		deleteAllSessions as deleteAllSessionsCommand,
		deleteSession as deleteSessionCommand,
		getSessionForResume,
		reopenSession as reopenSessionCommand,
		searchSessionHistoryFiltered,
		updateSessionTitle as updateSessionTitleCommand,
	} from '$lib/sessionCommands.ts';
	import { registerSessionLifecycleListener } from '$lib/events.ts';
	import { clearToolRunHistory, listToolRunHistory } from '$lib/toolRunCommands.ts';
	import { cancelSchedule, listSchedules } from '$lib/scheduleCommands.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import MaterialDatePicker from '$lib/MaterialDatePicker.svelte';
	import { openContextMenu } from '$lib/contextMenuStore.ts';
	import SessionHistory from './SessionHistory.svelte';
	import MemoryCenter from './MemoryCenter.svelte';
	import ToolRunCenter from '$lib/ToolRunCenter.svelte';
	import ScheduleCenter from '$lib/ScheduleCenter.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceSectionHeader from '$lib/WorkspaceSectionHeader.svelte';
	import type { TauriCommandRequest } from '$lib/contracts/generatedCommands.ts';
	import type { SessionHistoryStatusFilterInput } from '$lib/contracts/generatedCommands.ts';
	import type {
		Fact,
		FactProposal,
		FactProposalCursor,
		FactProposalDecision,
		MemoryFactCursor,
		MemoryFactSourceFilter,
		MemoryRecallFilter,
		MemoryRecallKind,
		MemoryRecallState,
	} from '$lib/contracts/memory.ts';
	import type { ToolRunPayload } from '$lib/contracts/toolRun.ts';
	import type { SchedulePlanPayload } from '$lib/contracts/schedule.ts';
	import type { SessionHistoryRow } from '$lib/contracts/sessionHistory.ts';
	import type { ContextMenuItem } from '$lib/contextMenuStore.ts';

	const MEMORY_TAB_IDS = ['sessions', 'tasks', 'memory'] as const;
	type MemoryTabId = (typeof MEMORY_TAB_IDS)[number];
	const MEMORY_TAB_LABELS: Record<MemoryTabId, string> = {
		sessions: '会话历史',
		tasks: '任务历史',
		memory: '长期记忆',
	};

	interface Props extends Pick<
		ComponentProps<typeof ToolRunCenter>,
		| 'runningBackgroundToolRuns'
		| 'toolRunStatusLabel'
		| 'sessionTitleFor'
		| 'toolRunDuration'
		| 'onOpenSession'
		| 'onCancel'
	> {
		isVisible?: boolean;
		onNewSession?: () => void;
	}

	let {
		isVisible = true,
		onNewSession = () => {},
		runningBackgroundToolRuns = [],
		toolRunStatusLabel = (status) => status || '',
		sessionTitleFor = () => '',
		toolRunDuration = () => '',
		onOpenSession = () => {},
		onCancel = () => {},
	}: Props = $props();

	let sessions = $state<SessionHistoryRow[]>([]);
	let searchQuery = $state('');
	let searchTimer: ReturnType<typeof setTimeout> | null = null;
	let deleteTarget = $state<SessionHistoryRow | null>(null);
	let showClearDialog = $state(false);
	let showClearTasksDialog = $state(false);
	let showClearMemoryDialog = $state(false);
	let clearingTasks = $state(false);
	let clearingMemory = $state(false);
	let offset = $state(0);
	let totalCount = $state(0);
	let loading = $state(false);
	let hasMore = $state(true);
	let loadSessionsSeq = 0;
	let loadFactsSeq = 0;
	let loadFactProposalsSeq = 0;
	let loadTaskHistorySeq = 0;
	const PAGE_SIZE = 50;
	let statusFilter = $state<'' | SessionHistoryStatusFilterInput>('');
	let startDate = $state('');
	let endDate = $state('');
	let showDateFilter = $state(false);
	let editingTitle = $state<string | null>(null);
	let renameValue = $state('');
	function isMemoryTabId(value: string | null): value is MemoryTabId {
		return value !== null && MEMORY_TAB_IDS.some((tabId) => tabId === value);
	}
	function memoryTabFromUrl(): MemoryTabId {
		const section = get(page).url.searchParams.get('section');
		return isMemoryTabId(section) ? section : 'sessions';
	}
	let activeTab = $state<MemoryTabId>(memoryTabFromUrl());
	const memoryTabs = MEMORY_TAB_IDS.map((id) => ({ id, label: MEMORY_TAB_LABELS[id] }));
	let toolRunHistory = $state<ToolRunPayload[]>([]);
	let toolRunHistoryLoading = $state(false);
	let toolRunHistoryFailed = $state(false);
	let schedules = $state<SchedulePlanPayload[]>([]);
	let schedulesLoading = $state(false);
	let schedulesFailed = $state(false);
	let memoryRecall = $state<MemoryRecallState>({
		query: '',
		kind: 'all',
		results: [],
		loading: false,
		searched: false,
	});
	let memoryRecallRequestSeq = 0;
	let facts = $state<Fact[]>([]);
	let factsLoaded = $state(false);
	let factsLoadingMore = $state(false);
	let factCursor = $state<MemoryFactCursor | null>(null);
	let factsHasMore = $derived(factCursor !== null);
	let factProposals = $state<FactProposal[]>([]);
	let factProposalsLoaded = $state(false);
	let factProposalsLoadingMore = $state(false);
	let factProposalCursor = $state<FactProposalCursor | null>(null);
	let factProposalsHasMore = $derived(factProposalCursor !== null);
	let busyProposalId = $state<string | null>(null);
	let factSourceFilter = $state<MemoryFactSourceFilter>('');
	let newFact = $state<{ predicate: string; object: string; tags: string; validFrom: string; validTo: string }>({
		predicate: '',
		object: '',
		tags: '',
		validFrom: '',
		validTo: '',
	});
	let addingFact = $state(false);
	const STATUS_OPTIONS: Array<{ value: '' | SessionHistoryStatusFilterInput; label: string }> = [
		{ value: '', label: '全部状态' },
		{ value: 'completed', label: '已完成' },
		{ value: 'paused', label: '已暂停' },
		{ value: 'error', label: '错误' },
	];
	const FACT_SOURCE_OPTIONS: Array<{ value: MemoryFactSourceFilter; label: string }> = [
		{ value: '', label: '全部来源' },
		{ value: 'user', label: '手动' },
		{ value: 'inferred', label: '推断' },
	];
	const todayISO = $derived.by(() => {
		const now = new Date();
		return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
	});
	let unlistenLifecycle: { dispose: () => void } | null = null;
	let lifecycleViewDisposed = false;
	const sessionsRefresh = createSessionRefreshScheduler(() => loadSessionsNow());

	onMount(async () => {
		const lifecycleListener = await registerSessionLifecycleListener(
			({ payload }) => {
				if (payload.type === 'title_updated') {
					sessions = sessions.map((session) =>
						session.sessionId === payload.sessionId
							? { ...session, title: payload.title }
							: session,
					);
				}
				sessionsRefresh.schedule();
			},
			{ tag: 'memory' },
		);
		if (lifecycleViewDisposed) {
			lifecycleListener.dispose();
			return;
		}
		unlistenLifecycle = lifecycleListener;
		await sessionsRefresh.refresh();
	});
	onDestroy(() => {
		lifecycleViewDisposed = true;
		memoryRecallRequestSeq += 1;
		if (searchTimer) clearTimeout(searchTimer);
		sessionsRefresh.dispose();
		unlistenLifecycle?.dispose();
		unlistenLifecycle = null;
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
		if (activeTab !== 'memory') return;
		loadFactProposals();
	});
	$effect(() => {
		if (activeTab !== 'tasks') return;
		refreshTaskHistory();
	});

	async function refreshTaskHistory() {
		await Promise.all([loadTaskHistory(), loadSchedules()]);
	}

	async function loadTaskHistory() {
		const sequence = ++loadTaskHistorySeq;
		toolRunHistoryLoading = true;
		toolRunHistoryFailed = false;
		try {
			const rows = await listToolRunHistory(100);
			if (sequence !== loadTaskHistorySeq) return;
			toolRunHistory = rows;
		} catch (e) {
			if (sequence !== loadTaskHistorySeq) return;
			toolRunHistory = [];
			toolRunHistoryFailed = true;
			reportError(e, { context: 'MemoryView', message: '加载任务历史失败', log: false });
		} finally {
			if (sequence === loadTaskHistorySeq) toolRunHistoryLoading = false;
		}
	}

	async function loadSchedules() {
		schedulesLoading = true;
		schedulesFailed = false;
		try {
			schedules = await listSchedules();
		} catch (e) {
			schedules = [];
			schedulesFailed = true;
			reportError(e, { context: 'MemoryView', message: '加载定时任务失败', log: false });
		} finally {
			schedulesLoading = false;
		}
	}

	async function handleCancelSchedule(scheduleId: string, sessionId: string) {
		try {
			if (!(await cancelSchedule(scheduleId, sessionId))) {
				await loadSchedules();
				addNotification('定时计划未取消，已重新同步状态', 'warning', 2500);
				return;
			}
			await loadSchedules();
		} catch (error) {
			reportError(error, { context: 'MemoryView', message: '取消定时计划失败', log: false });
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

	function filterParams(
		extra: Pick<TauriCommandRequest<'search_session_history_filtered'>, 'limit' | 'offset'>,
	) {
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
	function requestDelete(session: SessionHistoryRow) {
		deleteTarget = session;
	}
	async function loadSessionsNow() {
		const sequence = ++loadSessionsSeq;
		loading = true;
		try {
			const results = await searchSessionHistoryFiltered(
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
			const more = await searchSessionHistoryFiltered(
				filterParams({ limit: PAGE_SIZE, offset }),
			);
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
	function handleStatusFilterChange(value: '' | SessionHistoryStatusFilterInput) {
		statusFilter = value;
		handleFilterChange();
	}
	function handleRecallKindChange(value: MemoryRecallFilter) {
		memoryRecallRequestSeq += 1;
		memoryRecall.loading = false;
		memoryRecall.kind = value;
		memoryRecall.results = [];
		memoryRecall.searched = false;
	}
	function handleRecallQueryChange(value: string) {
		if (memoryRecall.query === value) return;
		memoryRecallRequestSeq += 1;
		memoryRecall.loading = false;
		memoryRecall.query = value;
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
	async function resumeSession(session: SessionHistoryRow) {
		try {
			const wasError = isErrorStatus(session.status);
			const runEndStatus: SessionRunEndStatus | undefined =
				session.status === 'paused'
					? 'paused'
					: session.status === 'completed'
						? 'completed'
						: wasError
							? 'error'
							: undefined;
			// Opening an errored session is read-only. Reopening it here used
			// to change the in-memory status to Paused before the chat could render,
			// which hid the actual failure state. Continue/retry performs the
			// explicit transition when the user asks for it.
			if (!wasError) await reopenSessionCommand({ sessionId: session.sessionId });
			const result = await getSessionForResume({ sessionId: session.sessionId });
			appSessionReducer.dispatch({
				type: 'session/messages/resume-loaded',
				sessionId: session.sessionId,
				messages: buildResumeMessages(result),
				interactions: result.interactions,
				preserveRequestIds: appSessionReducer.getPendingInteractionRequestIds(
					session.sessionId,
				),
				usage: result.usage,
				llmUsage: result.llm_usage,
			});
			sessionResumeTargetStore.set({
				sessionId: session.sessionId,
				inputText: session.input_text,
				title: session.title,
				runEndStatus,
				runEndOutcome:
					result.session.run_end_kind === 'turn_completed'
						? 'turn_completed'
						: undefined,
				runEndReason: result.session.run_end_reason || '',
				wasError,
				errorReason: wasError
					? result.session.run_end_reason ||
						appSessionReducer.getSessionErrorReason(session.sessionId)
					: '',
			});
			await goto('/');
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '加载会话详情失败', log: false });
		}
	}
	async function deleteSession(sessionId: string) {
		try {
			await deleteSessionCommand({ sessionId });
			sessions = sessions.filter((session) => session.sessionId !== sessionId);
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
	async function deleteAllSessions() {
		try {
			const count = await deleteAllSessionsCommand();
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
				message: '删除全部会话失败',
				log: false,
			});
		}
		showClearDialog = false;
	}
	async function clearTaskHistory() {
		if (clearingTasks) return;
		clearingTasks = true;
		try {
			const count = await clearToolRunHistory();
			await refreshTaskHistory();
			addNotification(`已清空 ${count} 条任务历史`, 'success', 3000);
		} catch (error) {
			reportError(error, { context: 'MemoryView', message: '清空任务历史失败', log: false });
		} finally {
			clearingTasks = false;
			showClearTasksDialog = false;
		}
	}
	async function clearMemoryFacts() {
		if (clearingMemory) return;
		clearingMemory = true;
		try {
			const count = await clearFactsCommand();
			loadFactsSeq += 1;
			facts = [];
			factsLoaded = true;
			factsLoadingMore = false;
			factCursor = null;
			loadFactProposalsSeq += 1;
			factProposals = [];
			factProposalsLoaded = true;
			factProposalsLoadingMore = false;
			factProposalCursor = null;
			clearMemoryRecall();
			addNotification(`已清空长期记忆和待审核推断（移除 ${count} 条事实）`, 'success', 3000);
		} catch (error) {
			reportError(error, { context: 'MemoryView', message: '清空长期记忆失败', log: false });
		} finally {
			clearingMemory = false;
			showClearMemoryDialog = false;
		}
	}
	function displayTitle(session: SessionHistoryRow) {
		if (session.title) return session.title;
		const text = session.input_text || '';
		const match = text.match(/^[^。！？\n.!?]+[。！？.!?]?/);
		return (match ? match[0].trim() : text.trim()) || '未命名会话';
	}
	function startEdit(session: SessionHistoryRow) {
		editingTitle = session.sessionId;
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
			const session = sessions.find((item) => item.sessionId === sessionId);
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
	function openCtxMenu(event: MouseEvent, session: SessionHistoryRow) {
		openContextMenu(event, buildContextMenuItems(session));
	}
	function buildContextMenuItems(session: SessionHistoryRow): ContextMenuItem[] {
		return [
			{ id: 'open', label: '打开', icon: 'open', action: () => resumeSession(session) },
			{ id: 'rename', label: '重命名', icon: 'edit', action: () => startEdit(session) },
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
	async function loadFacts() {
		const sequence = ++loadFactsSeq;
		factsLoaded = false;
		factsLoadingMore = false;
		facts = [];
		factCursor = null;
		try {
			const page = await listFacts({
				source: factSourceFilter || null,
				cursor: null,
				limit: PAGE_SIZE,
			});
			if (sequence !== loadFactsSeq) return;
			facts = page.facts;
			factCursor = page.nextCursor;
			factsLoaded = true;
		} catch (error) {
			if (sequence !== loadFactsSeq) return;
			facts = [];
			factCursor = null;
			factsLoaded = true;
			reportError(error, { context: 'MemoryView', message: '加载事实失败', log: false });
		}
	}
	async function loadMoreFacts() {
		if (!factCursor || factsLoadingMore) return;
		const sequence = loadFactsSeq;
		const cursor = factCursor;
		factsLoadingMore = true;
		try {
			const page = await listFacts({
				source: factSourceFilter || null,
				cursor,
				limit: PAGE_SIZE,
			});
			if (sequence !== loadFactsSeq) return;
			facts = [...facts, ...page.facts];
			factCursor = page.nextCursor;
		} catch (error) {
			if (sequence !== loadFactsSeq) return;
			reportError(error, { context: 'MemoryView', message: '加载更多事实失败', log: false });
		} finally {
			if (sequence === loadFactsSeq) factsLoadingMore = false;
		}
	}
	async function loadFactProposals() {
		const sequence = ++loadFactProposalsSeq;
		factProposalsLoaded = false;
		factProposalsLoadingMore = false;
		factProposals = [];
		factProposalCursor = null;
		try {
			const page = await listFactProposals({ cursor: null, limit: PAGE_SIZE });
			if (sequence !== loadFactProposalsSeq) return;
			factProposals = page.proposals;
			factProposalCursor = page.nextCursor;
			factProposalsLoaded = true;
		} catch (error) {
			if (sequence !== loadFactProposalsSeq) return;
			factProposals = [];
			factProposalCursor = null;
			factProposalsLoaded = true;
			reportError(error, { context: 'MemoryView', message: '加载待审核事实失败', log: false });
		}
	}
	async function loadMoreFactProposals() {
		if (!factProposalCursor || factProposalsLoadingMore) return;
		const sequence = loadFactProposalsSeq;
		const cursor = factProposalCursor;
		factProposalsLoadingMore = true;
		try {
			const page = await listFactProposals({ cursor, limit: PAGE_SIZE });
			if (sequence !== loadFactProposalsSeq) return;
			factProposals = [...factProposals, ...page.proposals];
			factProposalCursor = page.nextCursor;
		} catch (error) {
			if (sequence !== loadFactProposalsSeq) return;
			reportError(error, { context: 'MemoryView', message: '加载更多待审核事实失败', log: false });
		} finally {
			if (sequence === loadFactProposalsSeq) factProposalsLoadingMore = false;
		}
	}
	async function reviewFactProposal(proposalId: string, decision: FactProposalDecision) {
		if (busyProposalId !== null) return;
		busyProposalId = proposalId;
		try {
			const result = await reviewFactProposalCommand({ proposalId, decision });
			if (result.status === 'accepted') {
				addNotification(
					result.fact_written ? '推断事实已加入长期记忆' : '提案已接受，相关事实已存在，未重复添加',
					'success',
					3000,
				);
			} else if (result.status === 'rejected') {
				addNotification('已拒绝推断事实', 'success', 2500);
			} else {
				addNotification('该提案已处理，审核队列已刷新', 'info', 3000);
			}
			await Promise.all([loadFactProposals(), loadFacts()]);
		} catch (error) {
			reportError(error, { context: 'MemoryView', message: '审核推断事实失败', log: false });
		} finally {
			busyProposalId = null;
		}
	}
	function handleFactSourceFilterChange(value: MemoryFactSourceFilter) {
		if (value === factSourceFilter) return;
		factSourceFilter = value;
		loadFactsSeq += 1;
		facts = [];
		factsLoaded = false;
		factsLoadingMore = false;
		factCursor = null;
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
				validFrom: newFact.validFrom.trim() || null,
				validTo: newFact.validTo.trim() || null,
			});
			newFact = { predicate: '', object: '', tags: '', validFrom: '', validTo: '' };
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
			loadFactsSeq += 1;
			factsLoadingMore = false;
			facts = facts.filter((fact) => fact.factId !== factId);
		} catch (e) {
			reportError(e, { context: 'MemoryView', message: '删除事实失败', log: false });
		}
	}
	async function runRecall() {
		const query = memoryRecall.query.trim();
		if (!query) return;
		const sequence = ++memoryRecallRequestSeq;
		const kindFilter = memoryRecall.kind;
		memoryRecall.loading = true;
		try {
			const kinds: MemoryRecallKind[] =
				kindFilter === 'all' ? ['fact', 'episode'] : [kindFilter];
			const limit = kindFilter === 'all' ? 5 : 10;
			const resultGroups = await Promise.all(
				kinds.map(async (kind) => {
					const results = (await recallMemory({ query, kind, limit })) || [];
					return results.map((result) => ({ ...result, kind }));
				}),
			);
			if (sequence !== memoryRecallRequestSeq) return;
			memoryRecall.results = resultGroups.flat();
			memoryRecall.searched = true;
		} catch (e) {
			if (sequence !== memoryRecallRequestSeq) return;
			memoryRecall.results = [];
			memoryRecall.searched = true;
			reportError(e, { context: 'MemoryView', message: '记忆检索失败', log: false });
		} finally {
			if (sequence === memoryRecallRequestSeq) memoryRecall.loading = false;
		}
	}
	function clearMemoryRecall() {
		memoryRecallRequestSeq += 1;
		memoryRecall.query = '';
		memoryRecall.results = [];
		memoryRecall.loading = false;
		memoryRecall.searched = false;
	}
</script>

<div class="memory-page">
	<WorkspacePageHeader title="历史" description="回顾会话、任务和长期记忆。" />
	<div class="memory-layout workspace-secondary-layout responsive-layout-transition">
		<aside
			class="memory-sidebar workspace-secondary-sidebar responsive-layout-panel"
			aria-label="历史分类"
		>
			<MaterialTabs
				tabs={memoryTabs}
				activeTab={activeTab ?? 'sessions'}
				onNavigate={selectMemoryTab}
				ariaLabel="历史分区"
				idPrefix="memory-tab"
				panelId="memory-panel"
				className="workspace-secondary-tabs workspace-secondary-tabs--sidebar"
				{isVisible}
			/>
		</aside>
		<div class="memory-main workspace-secondary-main">
			{#key activeTab}
				<div
					id="memory-panel"
					class="memory-panel motion-surface-enter"
					role="tabpanel"
					aria-label={memoryTabs.find((tab) => tab.id === activeTab)?.label || '历史'}
				>
					{#if activeTab === 'sessions'}
						<WorkspaceSectionHeader
							title="会话历史"
							description="查看并继续过去的对话。"
						>
							{#snippet children()}
								<MaterialButton
									variant="danger"
									label="删除全部会话"
									onclick={() => (showClearDialog = true)}
									disabled={loading || totalCount === 0}
								/>
							{/snippet}
						</WorkspaceSectionHeader>
						<SessionHistory
							{sessions}
							{searchQuery}
							{totalCount}
							{statusFilter}
							statusOptions={STATUS_OPTIONS}
							{startDate}
							{endDate}
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
							description="查看后台工具调用和独立的定时计划。"
						>
							{#snippet children()}
								<RefreshButton
								loading={toolRunHistoryLoading || schedulesLoading}
									disabled={clearingTasks}
								onclick={refreshTaskHistory}
								/>
								<MaterialButton
									variant="danger"
									label={clearingTasks ? '正在清空…' : '清空历史'}
									ariaBusy={clearingTasks}
									onclick={() => (showClearTasksDialog = true)}
									disabled={clearingTasks ||
										toolRunHistoryLoading ||
										(toolRunHistory.length === 0 && runningBackgroundToolRuns.length === 0)}
								/>
							{/snippet}
						</WorkspaceSectionHeader>
						<ToolRunCenter
							{runningBackgroundToolRuns}
							{toolRunHistory}
							{toolRunHistoryLoading}
							{toolRunHistoryFailed}
							onRefreshTaskHistory={refreshTaskHistory}
							{toolRunStatusLabel}
							{sessionTitleFor}
							{toolRunDuration}
							{onOpenSession}
							{onCancel}
						/>
						<ScheduleCenter
							{schedules}
							loading={schedulesLoading}
							failed={schedulesFailed}
							sessionTitleFor={(sessionId) => sessionTitleFor({ sessionId })}
							onRefresh={loadSchedules}
							onCancel={handleCancelSchedule}
							onOpenSession={onOpenSession}
						/>
					{:else}
						<div class="memory-tools-view" aria-label="长期记忆">
							<WorkspaceSectionHeader
								title="长期记忆"
								description="管理已保存的长期事实，或检索过去的对话。"
							>
								{#snippet children()}
									<MaterialButton
										variant="danger"
										label="清空记忆"
										onclick={() => (showClearMemoryDialog = true)}
										disabled={!factsLoaded ||
										(!factProposalsLoaded ||
											factSourceFilter === '' && facts.length === 0 && factProposals.length === 0)}
									/>
								{/snippet}
							</WorkspaceSectionHeader>
							<MemoryCenter
								{facts}
								{factsLoaded}
								{factsHasMore}
								{factsLoadingMore}
								{factProposals}
								{factProposalsLoaded}
								{factProposalsHasMore}
								{factProposalsLoadingMore}
								{busyProposalId}
								{factSourceFilter}
								factSourceOptions={FACT_SOURCE_OPTIONS}
								{newFact}
								{addingFact}
								{memoryRecall}
								onRecallKindChange={handleRecallKindChange}
								onRecallQueryChange={handleRecallQueryChange}
								onRunRecall={runRecall}
								onClearRecall={clearMemoryRecall}
								onFactSourceFilterChange={handleFactSourceFilterChange}
								onLoadMoreFacts={loadMoreFacts}
								onLoadMoreFactProposals={loadMoreFactProposals}
								onReviewFactProposal={reviewFactProposal}
								onAddFact={addFact}
								onDeleteFact={deleteFact}
							/>
						</div>
					{/if}
				</div>
			{/key}
		</div>
	</div>
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
				if (deleteTarget) deleteSession(deleteTarget.sessionId);
			}}
		/>
	{/snippet}
</MaterialDialog>
<MaterialDialog
	open={showClearDialog}
	onClose={() => (showClearDialog = false)}
	title="删除全部会话"
>
	{#snippet children()}<p class="dialog-text">
			将永久删除全部会话记录（长期事实不受影响）。此操作不可撤销。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton variant="text" label="取消" onclick={() => (showClearDialog = false)} />
		<MaterialButton variant="danger" label="删除全部会话" onclick={deleteAllSessions} />
	{/snippet}
</MaterialDialog>
<MaterialDialog
	open={showClearTasksDialog}
	onClose={() => (showClearTasksDialog = false)}
	title="清空任务历史"
>
	{#snippet children()}<p class="dialog-text">
			将永久删除所有已结束的后台任务和定时任务记录。运行中、等待中以及尚未写入会话的任务结果会保留。此操作不可撤销。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="取消"
			onclick={() => (showClearTasksDialog = false)}
			disabled={clearingTasks}
		/>
		<MaterialButton
			variant="danger"
			label={clearingTasks ? '清空中…' : '清空历史'}
			onclick={clearTaskHistory}
			disabled={clearingTasks}
		/>
	{/snippet}
</MaterialDialog>
<MaterialDialog
	open={showClearMemoryDialog}
	onClose={() => (showClearMemoryDialog = false)}
	title="清空长期记忆"
>
	{#snippet children()}<p class="dialog-text">
			将永久删除所有已保存的长期事实，包括手动添加和自动提取的事实；会话历史不会受影响。此操作不可撤销。
		</p>{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="取消"
			onclick={() => (showClearMemoryDialog = false)}
			disabled={clearingMemory}
		/>
		<MaterialButton
			variant="danger"
			label={clearingMemory ? '清空中…' : '清空记忆'}
			onclick={clearMemoryFacts}
			disabled={clearingMemory}
		/>
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
	.memory-main,
	.memory-sidebar {
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
	@media (min-width: 840px) {
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
	@media screen and (orientation: landscape) and (max-height: 560px) {
		:global(.workspace-surface) {
			padding: var(--md-sys-space-sm);
		}
		:global(.workspace-secondary-sidebar) {
			top: var(--md-sys-space-xs);
		}
	}
</style>
