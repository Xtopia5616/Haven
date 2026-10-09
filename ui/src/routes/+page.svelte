<script lang="ts">
	import { isSessionInteractionRequest } from '$lib/contracts/app.ts';
	import logger from '$lib/logger.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { shouldShowContinueButton } from '$lib/continueSession.ts';
	import {
		isBusyStatus,
		isErrorStatus,
		isPausedStatus,
		sessionWaitingReason,
		waitingReasonLabel,
	} from '$lib/sessionStatus.ts';
	import { submitTranscript } from '$lib/submit.ts';
	import { createChatSessionController } from '$lib/chatSessionController.ts';
	import { createChatEventController } from '$lib/chatEventController.ts';
	import { createAskInteractionController } from '$lib/chatAskInteraction.ts';
	import { createChatSessionStartup } from '$lib/chatSessionStartup.ts';
	import { createChatViewController } from '$lib/chatViewController.ts';
	import { projectChatVisibleMessages } from '$lib/chatVisibleMessages.ts';
	import { createChatModelSync } from '$lib/chatModelSync.ts';
	import * as chatModelCommands from '$lib/chatModelCommands.ts';
	import { buildSessionSwitcherOptions } from '$lib/sessionSwitcher.ts';
	import { loadSettings } from '$lib/settingsCommands.ts';
	import { createChatModelOperations } from '$lib/chatModelOperations.ts';
	import { createStreamEventAggregator } from '$lib/streamAggregator.ts';
	import { registerPerformanceMetricsProvider } from '$lib/performanceMetrics.ts';
	import {
		deleteSession,
		listSessionHistory,
		getLatestSessionForResume,
		getSessionLineage,
		listRuntimeSessions,
		reopenSession,
	} from '$lib/sessionCommands.ts';
	import {
		appSessionReducer,
		createSessionSelectorStore,
		DRAFT_SESSION_ID,
	} from '$lib/sessionReducer.ts';
	import {
		buildTokenUsageDetails,
		buildTokenUsageTooltip,
	} from '$lib/sessionUsagePresentation.ts';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { get } from 'svelte/store';
	import { invoke, isTauri } from '$lib/tauri.ts';
	import {
		activeSessionStatusLabelStore,
		reactExecutionPhaseStore,
		reactExecutionPhaseForSession,
		updateReactExecutionPhase,
	} from '$lib/sessionRuntimeStore.ts';
	import { addNotification } from '$lib/notificationStore.ts';
	import {
		sessionResumeTargetStore,
		NEW_SESSION_INTENT_STORAGE_KEY,
		newSessionIntentStore,
	} from '$lib/sessionIntentStore.ts';
	import {
		refreshToolRuns,
		refreshSessionToolRuns,
		toolRunStore,
		sessionToolRunStore,
		setActiveSessionToolRun,
	} from '$lib/toolRunStore.ts';
	import { mediaPlanStore } from '$lib/mediaPlanStore.ts';
	import { syncStore } from '$lib/syncStore.ts';
	import { dragScroll } from '$lib/dragScroll.ts';
	import RollbackDialog from '$lib/RollbackDialog.svelte';
	import {
		closeContextMenu as closeGlobalContextMenu,
		openContextMenuAt,
	} from '$lib/contextMenu.ts';
	import SessionToolbar from '$lib/SessionToolbar.svelte';
	import SessionRail from '$lib/SessionRail.svelte';
	import ModelToolbar from '$lib/ModelToolbar.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import SessionHeader from '$lib/SessionHeader.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SessionTimeline from '$lib/SessionTimeline.svelte';
	import Composer from '$lib/Composer.svelte';
	import PendingInteractionsMenu from '$lib/PendingInteractionsMenu.svelte';
	import { requestConfirmationOpen } from '$lib/interactionPresentationStore.ts';
	import type {
		SessionAction,
		SessionMessage,
		SessionSummary,
		SessionRunEndNotice,
		SessionRunEndStatus,
		SessionTokenStats,
	} from '$lib/sessionReducer.ts';
	import type { SessionTokenStatsView } from '$lib/sessionUsagePresentation.ts';
	import type { SessionLlmUsage } from '$lib/contracts/sessionHistory.ts';
	import type { ChatModelOption } from '$lib/chatModelOperations.ts';
	import type {
		SessionHistoryRow,
		SessionLineageResponse,
	} from '$lib/contracts/sessionHistory.ts';
	import type { ToolRunPayload, ToolRunStatus } from '$lib/contracts/toolRun.ts';
	import type { AgentMediaPlanPayload } from '$lib/contracts/agent.ts';
	import type {
		ReasoningEffortSelectionInput,
		WebSearchModeInput,
	} from '$lib/contracts/generatedCommands.ts';
	import type { ChatFileAttachment, ChatImageAttachment } from '$lib/chatAttachmentTypes.ts';
	import type { SessionMessageContextMenuRequest } from '$lib/sessionTimeline.ts';
	import type { ReActExecutionPhaseSnapshot } from '$lib/sessionRuntimeStore.ts';

	let chatPageEl = $state<HTMLElement | null>(null);
	let inputRouterRef = $state<{ setDraft: (text: string) => void } | null>(null);
	let recentHistorySessions = $state<SessionHistoryRow[]>([]);
	let historyRefreshSeq = 0;
	let sessionLineage = $state<{
		parent: SessionSummary | null;
		children: SessionSummary[];
	} | null>(null);
	let sessionLineageLoading = $state(false);
	let sessionLineageError = $state(false);
	let sessionLineageSeq = 0;

	// Attachment & compression limits for the input router, loaded from the
	// persisted [context_limits] config (editable on the settings "媒体"
	// page). Defaults mirror the backend config until settings arrive.
	let inputLimits = $state({
		maxImages: 4,
		maxImageBytes: 10 * 1024 * 1024,
		maxImageDim: 1568,
		jpegQuality: 0.85,
		maxFiles: 5,
		maxFileBytes: 20 * 1024 * 1024,
	});
	let initialLoading = $state(true);
	let deleteTarget = $state<{ sessionId: string; title: string } | null>(null);
	let deletingSession = $state(false);
	const sessionReducer = appSessionReducer;
	const currentReducerState = sessionReducer.snapshot();
	const emptySessionMessages: SessionMessage[] = [];
	const emptySessionLlmUsage: SessionLlmUsage[] = [];
	const sessionsStore = createSessionSelectorStore((state) => state.sessions);
	const activeSessionIdStore = createSessionSelectorStore((state) => state.activeSessionId);
	const interactionsStore = createSessionSelectorStore((state) => state.interactions);
	const activeSessionMessagesStore = createSessionSelectorStore((state) => {
		const sessionId = state.activeSessionId || DRAFT_SESSION_ID;
		return state.messages[sessionId] ?? emptySessionMessages;
	});
	const activeSessionTokenStatsStore = createSessionSelectorStore((state) =>
		state.activeSessionId ? (state.tokenStats[state.activeSessionId] ?? null) : null,
	);
	const activeSessionLlmUsageStore = createSessionSelectorStore((state) =>
		state.activeSessionId
			? (state.llmUsage[state.activeSessionId] ?? emptySessionLlmUsage)
			: emptySessionLlmUsage,
	);
	const sessionRunEndNoticeStore = createSessionSelectorStore((state) => state.runEndNotice);
	let sessions = $state(currentReducerState.sessions);
	let activeSessionId = $state(currentReducerState.activeSessionId);
	let interactionDict = $state(currentReducerState.interactions);
	let dismissedAskIds = $state(new Set<string>());
	let activeSessionMessages = $state(
		currentReducerState.messages[currentReducerState.activeSessionId || DRAFT_SESSION_ID] ??
			emptySessionMessages,
	);
	let sessionRunEndNotice = $state(currentReducerState.runEndNotice);

	function dispatchSession(action: SessionAction) {
		sessionReducer.dispatch(action);
	}

	$effect(() => syncStore(sessionsStore, (next) => (sessions = next)));
	$effect(() => syncStore(activeSessionIdStore, (next) => (activeSessionId = next)));
	$effect(() => syncStore(interactionsStore, (next) => (interactionDict = next)));
	$effect(() => syncStore(activeSessionMessagesStore, (next) => (activeSessionMessages = next)));
	$effect(() => syncStore(sessionRunEndNoticeStore, (next) => (sessionRunEndNotice = next)));
	// Interaction requests are shared by the ask cards and confirmation modal.
	// The modal keeps only its current presentation id; pending requests remain
	// owned by SessionReducer so ask/confirm/scheduled-confirm cannot drift.
	const pendingInteractions = $derived(
		Object.values(interactionDict).filter(
			(request) => isSessionInteractionRequest(request) && request.status === 'pending',
		),
	);
	const pendingAskInteractions = $derived(
		pendingInteractions.filter((request) => request.kind === 'ask'),
	);
	const pendingInteractionItems = $derived.by(() =>
		[...pendingInteractions]
			.filter(isSessionInteractionRequest)
			.sort((left, right) => left.createdAt.localeCompare(right.createdAt))
			.map((request) => {
				const sessionTitle =
					sessions.find((session) => session.id === request.owner.sessionId)?.title ||
					request.owner.sessionId;
				if (request.kind === 'ask') {
					const question = sessionReducer
						.getMessages(request.owner.sessionId)
						.find((message) => message.id === request.id)?.content;
					return {
						id: request.id,
						kind: request.kind,
						title: `待回答 · ${sessionTitle}`,
						detail: question || 'Haven 正在等待你的回答',
					};
				}
				return {
					id: request.id,
					kind: request.kind,
					title: `权限确认 · ${sessionTitle}`,
					detail: request.summary || request.toolName || '等待你的许可',
				};
			}),
	);
	const askAwaiting = $derived(pendingAskInteractions.length > 0);
	const askHasOptions = $derived(
		pendingAskInteractions.some((request) => request.options.length > 0),
	);
	let rollbackDialog = $state<{
		open: boolean;
		stepNumber: number | null;
		role: SessionMessageContextMenuRequest['role'] | null;
		content: string;
		msgId: string;
	}>({
		open: false,
		stepNumber: null,
		role: null,
		content: '',
		msgId: '',
	});
	let rollbackLoading = $state(false);

	// Model switcher state: configured chat-capable model profiles and the
	// current primary profile selected by the Chat request route.
	let modelMenuOpen = $state(false);
	let sessionMenuOpen = $state(false);
	let modelOptions = $state<ChatModelOption[]>([]);
	let currentModelName = $state('');
	let currentModelId = $state('');
	let currentEffort = $state('');
	// Provider built-in web search mode ("off" | "auto" | "always").
	// Defaults to off (opt-in); "auto" lets the model decide when to search.
	let currentWebSearch = $state('off');
	/** Selected chat model provider wire style supports built-in 联网搜索. */
	let webSearchSupported = $state(false);
	/** Normalized wire style of the selected chat model provider. */
	let currentApiStyle = $state('openai-chat');
	// The configured recording hotkey binding, loaded from settings and kept
	// in sync via `hotkey:rebind` so placeholders show the real value.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	// Read usage for the active session directly from the reducer-owned state.
	let tokenStats = $state<SessionTokenStats | null>(
		currentReducerState.activeSessionId
			? (currentReducerState.tokenStats[currentReducerState.activeSessionId] ?? null)
			: null,
	);
	$effect(() => syncStore(activeSessionTokenStatsStore, (next) => (tokenStats = next)));

	// Per-LLM-call usage detail for the active session (restored from the
	// persisted `llm_usage` when a session resumes). Used by the
	// session-level token tooltip and call count.
	let llmUsage = $state<SessionLlmUsage[]>(
		currentReducerState.activeSessionId
			? (currentReducerState.llmUsage[currentReducerState.activeSessionId] ??
					emptySessionLlmUsage)
			: emptySessionLlmUsage,
	);
	$effect(() => syncStore(activeSessionLlmUsageStore, (next) => (llmUsage = next)));

	function buildTokenTooltip(stats: SessionTokenStatsView) {
		return buildTokenUsageTooltip(stats, llmUsage);
	}

	// Send/interrupt merged button: text takes priority (always send); with no
	// text and the agent actively generating output the button interrupts the
	// current output while keeping the session resumable.
	let reactExecutionPhase = $state<ReActExecutionPhaseSnapshot>({
		sessionId: null,
		phase: 'idle',
	});
	let interruptPending = $state(false);
	$effect(() =>
		syncStore(reactExecutionPhaseStore, (v) => {
			reactExecutionPhase = v;
		}),
	);
	const activeSessionPhase = $derived(
		reactExecutionPhaseForSession(reactExecutionPhase, activeSessionId),
	);
	const isGenerating = $derived(
		activeSessionPhase === 'generating' ||
			activeSessionPhase === 'waiting_result' ||
			activeSessionPhase === 'waiting_response',
	);
	const sessionRunning = $derived(
		!!activeSessionId &&
			sessions.some((t) => t.id === activeSessionId && isBusyStatus(t.status)),
	);
	// Tooltip for the token widget. While the active session is still running
	// (streaming, tool-calling, or queued) more `agent:usage` events are
	// expected. A finished or history-opened session with no persisted
	// usage will never receive events, so show a neutral hint instead.
	const tokenStatsHint = $derived(isGenerating || sessionRunning ? '等待 LLM 统计' : '暂无统计');
	const tokenUsageDetails = $derived.by(() =>
		tokenStats ? buildTokenUsageDetails(tokenStats, llmUsage) : null,
	);
	// Compact layouts hide the session rail, so include persisted history as
	// well as live/paused sessions in the chat switcher.
	const menuSessions = $derived(buildSessionSwitcherOptions(sessions, recentHistorySessions));
	const showSessionMenu = $derived(menuSessions.length > 0);

	async function loadRecentHistory() {
		const sequence = ++historyRefreshSeq;
		try {
			const history = await listSessionHistory({ limit: 50, offset: 0 });
			if (!dead && sequence === historyRefreshSeq) recentHistorySessions = history || [];
		} catch (error) {
			if (!dead && sequence === historyRefreshSeq) {
				reportError(error, { context: '+page', message: '加载会话历史失败', log: false });
			}
		}
	}

	function mapLineageSession(
		session: SessionLineageResponse['children'][number],
	): SessionSummary {
		return {
			id: session.id,
			status: session.status,
			title: session.title || session.input_text,
			inputText: session.input_text,
		};
	}

	async function loadSessionLineage(sessionId: string | null) {
		const sequence = ++sessionLineageSeq;
		if (!sessionId) {
			sessionLineage = null;
			sessionLineageLoading = false;
			sessionLineageError = false;
			return;
		}
		sessionLineage = null;
		sessionLineageError = false;
		sessionLineageLoading = true;
		try {
			const lineage = await getSessionLineage({ sessionId });
			if (dead || sequence !== sessionLineageSeq || activeSessionId !== sessionId) return;
			sessionLineage = {
				parent: lineage.parent ? mapLineageSession(lineage.parent) : null,
				children: lineage.children.map(mapLineageSession),
			};
		} catch (error) {
			if (!dead && sequence === sessionLineageSeq) {
				sessionLineage = null;
				sessionLineageError = true;
				reportError(error, {
					context: '+page',
					message: '加载关联会话失败',
					log: false,
				});
			}
		} finally {
			if (!dead && sequence === sessionLineageSeq) sessionLineageLoading = false;
		}
	}

	// Live ToolRun registry (for "waiting on background" banner). Synced from
	// the global toolRunStore kept by +layout.
	let toolRunsById = $state<Record<string, ToolRunPayload>>({});
	$effect(() => syncStore(toolRunStore, (v) => (toolRunsById = v || {})));
	let sessionToolRunsById = $state<Record<string, Record<string, ToolRunPayload>>>({});
	$effect(() => syncStore(sessionToolRunStore, (v) => (sessionToolRunsById = v || {})));
	const sessionToolRuns = $derived.by(() => {
		if (!activeSessionId) return [];
		const toolRuns = new Map<string, ToolRunPayload>();
		for (const toolRun of Object.values(sessionToolRunsById[activeSessionId] || {})) {
			toolRuns.set(toolRun.toolRunId, toolRun);
		}
		for (const toolRun of Object.values(toolRunsById)) {
			if (toolRun.sessionId === activeSessionId) toolRuns.set(toolRun.toolRunId, toolRun);
		}
		return [...toolRuns.values()];
	});
	$effect(() => {
		const sessionId = activeSessionId;
		const persistedSessionId = sessionId === DRAFT_SESSION_ID ? null : sessionId;
		setActiveSessionToolRun(persistedSessionId);
		if (persistedSessionId && isTauri()) void refreshSessionToolRuns(persistedSessionId);
	});
	let mediaPlansBySession = $state<Record<string, AgentMediaPlanPayload[]>>({});
	$effect(() => syncStore(mediaPlanStore, (v) => (mediaPlansBySession = v || {})));
	const activeSessionStatus = $derived(
		activeSessionId ? sessions.find((t) => t.id === activeSessionId)?.status : undefined,
	);
	/** The backend's derived reason is authoritative; toolRuns only provide the count. */
	const awaitingBackground = $derived.by(() => {
		if (!activeSessionId || activeSessionStatus !== 'paused') return false;
		const session = sessions.find((item) => item.id === activeSessionId);
		return sessionWaitingReason(session) === 'background_task';
	});
	const awaitingBackgroundCount = $derived.by(() => {
		if (!awaitingBackground || !activeSessionId) return 0;
		return Object.values(toolRunsById).filter(
			(a) =>
				a &&
				a.kind !== 'scheduled' &&
				a.status === 'running' &&
				a.sessionId === activeSessionId,
		).length;
	});

	const effortOptions: Array<{ value: ReasoningEffortSelectionInput | ''; label: string }> = [
		{ value: '', label: '默认' },
		{ value: 'low', label: '低' },
		{ value: 'medium', label: '中' },
		{ value: 'high', label: '高' },
		// Vendor thinking off (DeepSeek/Kimi/Responses); omitted for plain OpenAI.
		{ value: 'off', label: '关闭' },
	];

	const webSearchOptionsAll: Array<{ value: WebSearchModeInput; label: string }> = [
		{ value: 'off', label: '关闭' },
		{ value: 'auto', label: '自动' },
		{ value: 'always', label: '总是' },
	];

	/** Gemini has no forced-search tool_choice; hide Always for that style. */
	let webSearchOptions = $derived(
		currentApiStyle === 'gemini'
			? webSearchOptionsAll.filter((o) => o.value !== 'always')
			: webSearchOptionsAll,
	);

	// Right-click context menu state
	let ctxMenu = $state<{
		stepNumber: number | null;
		content: string;
		role: SessionMessageContextMenuRequest['role'] | null;
		msgId: string;
		selectedContent: string;
	}>({
		stepNumber: null,
		content: '',
		role: null,
		msgId: '',
		selectedContent: '',
	});

	function handleContextMenu(ev: SessionMessageContextMenuRequest) {
		const next = {
			stepNumber: ev.stepNumber,
			content: ev.content,
			role: ev.role,
			msgId: ev.messageId,
			selectedContent: ev.selectedContent || '',
		};
		ctxMenu = next;
		openContextMenuAt(ev.x, ev.y, [
			{
				id: 'rollback',
				label: '回退到此消息',
				icon: 'rollback',
				action: handleCtxRollback,
			},
			{ id: 'copy', label: '复制', icon: 'copy', action: handleCtxCopy },
		]);
	}

	// Rollback: find step number from click context or parse from message id
	function getStepForCtxMenu() {
		if (ctxMenu.stepNumber != null) return ctxMenu.stepNumber;
		// For user messages, look forward in the message list to the next
		// assistant message that carries a stepNumber.
		if (ctxMenu.role === 'user' && ctxMenu.msgId) {
			const idx = messages.findIndex((m) => m.id === ctxMenu.msgId);
			if (idx >= 0) {
				const next = messages.slice(idx + 1).find((m) => m.stepNumber != null);
				if (next) return next.stepNumber;
			}
			// Fallback for an interrupted message that was never processed
			// (no step row and nothing after it — sent while the session was
			// erroring, or the app closed before the steering drained).
			// Target the step after the last completed one; the backend
			// discards just this message when no branch point covers it.
			const maxStep = messages.reduce(
				(acc, m) => (m.stepNumber != null ? Math.max(acc, m.stepNumber) : acc),
				0,
			);
			return maxStep + 1;
		}
		return null;
	}

	function handleCtxRollback() {
		const step = getStepForCtxMenu();
		if (step == null || ctxMenu.role == null) {
			addNotification('无法确定此消息对应的步骤', 'error', 3000);
			closeCtxMenu();
			return;
		}
		rollbackDialog = {
			open: true,
			stepNumber: step,
			role: ctxMenu.role,
			content: ctxMenu.content,
			msgId: ctxMenu.msgId,
		};
		closeCtxMenu();
	}

	async function handleCtxCopy() {
		const text = ctxMenu.selectedContent || ctxMenu.content;
		if (text) {
			try {
				await navigator.clipboard.writeText(text);
				addNotification('已复制', 'info', 1500);
			} catch (error) {
				reportError(error, { context: '+page', message: '复制失败' });
			}
		}
		closeCtxMenu();
	}

	function closeCtxMenu() {
		ctxMenu = {
			stepNumber: null,
			content: '',
			role: null,
			msgId: '',
			selectedContent: '',
		};
		closeGlobalContextMenu();
	}

	function handleWindowClick(e: MouseEvent) {
		const target = e.target instanceof Node ? e.target : null;
		if (modelMenuOpen) {
			const menu = document.querySelector('.model-menu');
			const btn = document.querySelector('.model-switch-btn');
			if (
				menu &&
				btn &&
				!(target && menu.contains(target)) &&
				!(target && btn.contains(target))
			) {
				modelMenuOpen = false;
			}
		}
		if (sessionMenuOpen) {
			const menu = document.querySelector('.session-menu');
			const btn = document.querySelector('.session-switch-btn');
			if (
				menu &&
				btn &&
				!(target && menu.contains(target)) &&
				!(target && btn.contains(target))
			) {
				sessionMenuOpen = false;
			}
		}
	}

	$effect(() => {
		if (sessionMenuOpen && menuSessions.length === 0) sessionMenuOpen = false;
	});

	// Merged into existing onMount/onDestroy below

	function newSession() {
		if (activeSessionId) {
			dispatchSession({ type: 'session/memory-cleared', sessionId: activeSessionId });
		}
		// 新会话 = explicit fresh start. While `newSessionIntentStore` is set, no
		// event-driven path may auto-assign an existing session (loadSessions
		// auto-assign, session:lifecycle(created), auto-restore), otherwise the next message
		// would append to the old session instead of starting a new one.
		// The intent is cleared only when the user's own submission creates a
		// session (submit.ts) or they explicitly switch to another session. Also
		// persisted to localStorage so the next app launch skips restoring the
		// previous session.
		newSessionIntentStore.set(true);
		if (browser) localStorage.setItem(NEW_SESSION_INTENT_STORAGE_KEY, '1');
		dispatchSession({ type: 'session/cleared' });
		sessionMenuOpen = false;
	}

	function toggleSessionMenu() {
		sessionMenuOpen = !sessionMenuOpen;
		if (sessionMenuOpen) {
			void loadRecentHistory();
			void loadSessionLineage(activeSessionId);
		}
	}

	// Terminal sessions are not in list_runtime_sessions, so drop their cached messages
	// when they are deactivated. A later switch reloads them from the database.
	function evictTerminalSessionMemory(sessionId: string | null) {
		chatSessionController.evictTerminalSessionMemory(sessionId);
	}

	// Owns chat-page event composition and listener registration lifetime.
	let chatEventController: ReturnType<typeof createChatEventController> | null = null;
	let messagesEl: HTMLElement | null | undefined = undefined;
	let autoFollow = $state(true);
	let dead = false;
	const chatViewController = createChatViewController({
		getMessagesElement: () => messagesEl,
		getAutoFollow: () => autoFollow,
		setAutoFollow: (follow) => (autoFollow = follow),
		isDisposed: () => dead,
	});
	const messages = $derived.by(() =>
		projectChatVisibleMessages(activeSessionMessages, interactionDict).filter(
			(message) =>
				message.type !== 'ask' || !message.awaiting || !dismissedAskIds.has(message.id),
		),
	);

	const activeRunFailed = $derived(
		!!activeSessionId &&
			sessionRunEndNotice?.sessionId === activeSessionId &&
			sessionRunEndNotice.status === 'error',
	);
	const activeSessionRunEndNotice = $derived(
		!!activeSessionId && sessionRunEndNotice?.sessionId === activeSessionId
			? sessionRunEndNotice
			: null,
	);
	const erroredSessionId = $derived(
		sessionRunEndNotice?.status === 'error' ? sessionRunEndNotice.sessionId : null,
	);
	let continuePending = $state(false);
	const showContinueButton = $derived(
		!!activeSessionId && shouldShowContinueButton(messages, activeRunFailed),
	);
	// Keep the affordance visible when a session ends with a user message, but do not
	// let it race a normal pending/running turn. `continue_session` is only a
	// retry operation for paused/error sessions.
	const continueDisabled = $derived(
		continuePending || (!activeRunFailed && !isPausedStatus(activeSessionStatus)),
	);

	// Auto-scroll to the newest message whenever messages change.
	$effect(() => {
		const _ = messages;
		if (messages.length > 0) {
			chatViewController.scrollToBottom();
		}
	});

	// When the active session changes (e.g. switching to a reviewed session or
	// creating a new session), re-enable follow and scroll to the bottom.
	$effect(() => {
		const _ = activeSessionId;
		chatViewController.setAutoFollow(true);
		chatViewController.scrollToBottom();
	});

	const streamEvents = createStreamEventAggregator({
		getActiveSessionId: () => activeSessionId,
		onActiveStream: (sessionId) => updateReactExecutionPhase(sessionId, 'generating'),
		dispatch: dispatchSession,
		getBlockIds: (sessionId, stepNumber, runId) =>
			sessionReducer.getBlockIds(sessionId, stepNumber, runId),
	});
	const { chunkHandler, clearStepBlockIds, flushChunksNow, metricsSnapshot } = streamEvents;
	const unregisterPerformanceMetricsProvider =
		registerPerformanceMetricsProvider(metricsSnapshot);

	// Configured Chat-route profiles and their settings synchronization live
	// outside the route component; this page only supplies Svelte state setters.
	let skipNextDefaultModelRefresh = false;
	const modelSync = createChatModelSync({
		isDead: () => dead,
		setModelOptions: (value) => {
			modelOptions = value;
		},
		setCurrentModelId: (value) => {
			currentModelId = value;
		},
		setCurrentModelName: (value) => {
			currentModelName = value;
		},
		setCurrentEffort: (value) => {
			currentEffort = value;
		},
		setCurrentWebSearch: (value) => {
			currentWebSearch = value;
		},
		setWebSearchSupported: (value) => {
			webSearchSupported = value;
		},
		setCurrentApiStyle: (value) => {
			currentApiStyle = value;
		},
	});
	const { applyDefaultModelFromSettings, refreshDefaultModelFromBackend } = modelSync;
	const modelOperations = createChatModelOperations({
		commands: chatModelCommands,
		setSkipNextDefaultModelRefresh: (skip) => {
			skipNextDefaultModelRefresh = skip;
		},
		setCurrentModelId: (value) => {
			currentModelId = value;
		},
		setCurrentModelName: (value) => {
			currentModelName = value;
		},
		setCurrentEffort: (value) => {
			currentEffort = value;
		},
		setCurrentWebSearch: (value) => {
			currentWebSearch = value;
		},
		setCurrentApiStyle: (value) => {
			currentApiStyle = value;
		},
		setWebSearchSupported: (value) => {
			webSearchSupported = value;
		},
		getEffortLabel: (value) =>
			effortOptions.find((option) => option.value === value)?.label || '默认',
		getWebSearchLabel: (value) =>
			webSearchOptionsAll.find((option) => option.value === value)?.label || '关闭',
		isWebSearchSupported: () => webSearchSupported,
		closeModelMenu: () => {
			modelMenuOpen = false;
		},
		notify: addNotification,
		reportError,
	});

	// The resume target set by the history page's "open session" flow must be
	// handled while the chat view is already mounted. `$effect` does NOT track
	// `get(store)` (svelte/store wraps the read in `untrack`), so a plain
	// `get(sessionResumeTargetStore)` here would only see the initial value and never
	// react to later history clicks. Subscribing via syncStore runs the callback
	// on every store change (and synchronously once with the current value).
	$effect(() =>
		syncStore(sessionResumeTargetStore, (value) => sessionStartup.processResumeTarget(value)),
	);

	// Measure the composer overlay and reserve the same clearance under messages.
	$effect(() => chatViewController.observeComposerClearance(chatPageEl, browser));

	onMount(async () => {
		// Hydrate the fresh-start intent from localStorage BEFORE any data
		// load: the store is in-memory only, but the intent survives app
		// restarts via `haven.no_auto_restore`. Without this, `loadSessions`
		// auto-assign would re-select the old session on restart and the
		// persisted intent would be silently defeated. The resumeTarget
		// branch below (an explicit user choice) clears it again if needed.
		sessionStartup.hydrateFreshSessionIntent();
		void loadRecentHistory();

		// Process resume target first so loadSessions won't overwrite
		// activeSessionId with a stale paused session whose messages are gone.
		const initialSessionResumeTarget = get(sessionResumeTargetStore);
		sessionStartup.processResumeTarget(initialSessionResumeTarget);

		// Register listeners BEFORE any async data load so session/streaming
		// events arriving while the page initializes are never missed.
		const eventController = createChatEventController({
			getActiveSessionId: () => activeSessionId,
			isFreshSessionIntent: () => get(newSessionIntentStore),
			adoptDraftMessages: (sessionId) => {
				const adopted = sessionReducer.getMessages(DRAFT_SESSION_ID).length > 0;
				dispatchSession({ type: 'session/messages/adopt-draft', sessionId });
				return adopted;
			},
			dispatchSession,
			getErroredSessionId: () => erroredSessionId,
			clearAskAwaiting: (sessionId) => {
				clearAskAwaiting(sessionId);
				if (sessionId) dispatchSession({ type: 'session/interactions-cleared', sessionId });
			},
			evictTerminalSessionMemory,
			clearStepBlockIds,
			flushChunksNow,
			updateSessionTitle: (sessionId, title) => {
				const index = sessions.findIndex((session) => session.id === sessionId);
				if (index >= 0) {
					dispatchSession({ type: 'session/title-updated', sessionId, title });
					// Keep the shell's task/status view in sync with the chat header
					// as soon as the generated title arrives.
				} else {
					// A title event can win the race with the initial session list load.
					// The persisted title will be picked up here.
					scheduleLoadSessions();
				}
			},
			scheduleLoadSessions: () => {
				scheduleLoadSessions();
				void loadRecentHistory();
			},
			chunkHandler,
			setHotkeyBinding: (binding) => {
				hotkeyBinding = binding;
			},
			getSkipNextDefaultModelRefresh: () => skipNextDefaultModelRefresh,
			clearSkipNextDefaultModelRefresh: () => {
				skipNextDefaultModelRefresh = false;
			},
			refreshDefaultModelFromBackend,
		});
		chatEventController = eventController;
		// Tauri listener registration is asynchronous. Complete it before any
		// restore/reopen call can trigger a confirmation, otherwise the event can
		// be emitted into the small registration gap and the modal never appears.
		await eventController.register();
		if (dead) return;

		// Load configured Chat profiles and the current route primary for the
		// toolbar. Fire-and-forget so loading settings never delays chat
		// rendering.
		loadSettings()
			.then((s) => {
				applyDefaultModelFromSettings(s);
				if (s?.hotkey?.key_binding) {
					hotkeyBinding = s.hotkey.key_binding;
				}
				const cl = s?.context_limits;
				if (cl) {
					inputLimits = {
						maxImages: cl.max_attachment_images ?? 4,
						maxImageBytes: cl.max_attachment_image_bytes ?? 10 * 1024 * 1024,
						maxImageDim: cl.max_attachment_image_dim_px ?? 1568,
						jpegQuality: cl.attachment_image_jpeg_quality ?? 0.85,
						maxFiles: cl.max_attachment_files ?? 5,
						maxFileBytes: cl.max_attachment_file_bytes ?? 20 * 1024 * 1024,
					};
				}
			})
			.catch((e) => {
				reportError(e, { context: '+page', message: '加载设置失败', log: false });
			});

		// Load the session list and auto-restore the last session in
		// parallel; chat renders as soon as its data arrives,
		// without waiting for `reopen_session` (a second IPC round-trip that
		// only makes the session resumable for follow-up messages).
		await sessionStartup.loadInitialSessions(initialSessionResumeTarget);
		if (dead) return;

		// Session just opened (history resume or auto-restore): scroll to
		// the real bottom, forcing the estimated content-visibility heights to
		// render first (see scrollToBottomAfterOpen).
		if (activeSessionId) {
			await tick();
			if (dead) return;
			chatViewController.scrollToBottomAfterOpen();
		}

		if (browser) {
			window.addEventListener('click', handleWindowClick);
		}
	});

	onDestroy(() => {
		dead = true;
		unregisterPerformanceMetricsProvider();
		// Flush any queued streaming chunks so the in-memory message store is
		// complete before the listeners are disposed (a re-entry to this page
		// merges the store with the DB copy).
		flushChunksNow();
		chatEventController?.dispose();
		sessionStartup.dispose();
		chatViewController.dispose();
		if (browser) {
			window.removeEventListener('click', handleWindowClick);
		}
	});

	const sessionStartup = createChatSessionStartup({
		reducer: sessionReducer,
		dispatch: dispatchSession,
		listRuntimeSessions,
		getLatestSessionForResume,
		reopenSession,
		refreshToolRuns,
		getFreshSessionIntent: () => get(newSessionIntentStore),
		setFreshSessionIntent: (value) => newSessionIntentStore.set(value),
		hasPersistedFreshSessionIntent: () =>
			browser && Boolean(localStorage.getItem(NEW_SESSION_INTENT_STORAGE_KEY)),
		clearPersistedFreshSessionIntent: () => {
			if (browser) localStorage.removeItem(NEW_SESSION_INTENT_STORAGE_KEY);
		},
		getPendingInteractionIds: (sessionId) => pendingInteractionIdsForSession(sessionId),
		evictTerminalSessionMemory,
		setInitialLoading: (loading) => (initialLoading = loading),
		deferResumeTargetClear: () => setTimeout(() => sessionResumeTargetStore.set(null), 0),
		warn: (message, error) => logger.warn('+page', message, error),
		reportError,
	});
	const { loadSessions, scheduleLoadSessions } = sessionStartup;

	const chatSessionController = createChatSessionController({
		invoke,
		submitTranscript: (text, options) => submitTranscript(text, options),
		reducer: sessionReducer,
		dispatch: dispatchSession,
		getActiveSessionId: () => sessionReducer.snapshot().activeSessionId,
		getSessionSnapshot: () => sessionReducer.snapshot().sessions,
		notify: addNotification,
		reportError,
		setInputDraft: (content) => inputRouterRef?.setDraft(content),
		loadSessions,
		clearStepBlockIds,
		setFreshSessionIntent: (value) => newSessionIntentStore.set(value),
		clearPersistedFreshSessionIntent: () => {
			if (browser) localStorage.removeItem(NEW_SESSION_INTENT_STORAGE_KEY);
		},
		setRollbackLoading: (loading) => (rollbackLoading = loading),
		closeRollbackDialog: () => {
			rollbackDialog = { open: false, stepNumber: null, role: null, content: '', msgId: '' };
		},
		closeSessionMenu: () => (sessionMenuOpen = false),
		setContinuePending: (pending) => (continuePending = pending),
		setInterruptPending: (pending) => (interruptPending = pending),
		setAutoFollow: (follow) => chatViewController.setAutoFollow(follow),
	});

	function submitMessage(
		text: string,
		images?: ChatImageAttachment[] | null,
		files?: ChatFileAttachment[] | null,
	) {
		return chatSessionController.submitMessage(text, images, files);
	}

	function confirmRollbackAction() {
		const stepNumber = rollbackDialog.stepNumber;
		const role = rollbackDialog.role;
		if (stepNumber == null || role == null) return;
		return chatSessionController.confirmRollbackAction({
			stepNumber,
			role,
			content: rollbackDialog.content,
			msgId: rollbackDialog.msgId,
		});
	}

	function pendingInteractionIdsForSession(sessionId: string) {
		return chatSessionController.pendingInteractionIdsForSession(sessionId);
	}

	async function switchToSession(sessionId: string) {
		const alreadyLoaded = sessionReducer
			.snapshot()
			.sessions.some((session) => session.id === sessionId);
		const historical = recentHistorySessions.find((session) => session.id === sessionId);
		if (!alreadyLoaded && historical) {
			if (isErrorStatus(historical.status)) {
				dispatchSession({
					type: 'session/retained-error',
					session: {
						id: historical.id,
						input: historical.input_text,
						inputText: historical.input_text,
						title: historical.title,
						status: 'error',
					},
				});
			} else {
				try {
					await reopenSession({ sessionId });
					await loadSessions();
				} catch (error) {
					reportError(error, { context: '+page', message: '恢复会话失败', log: false });
					return;
				}
			}
		}
		await chatSessionController.switchToSession(sessionId);
		const historicalRunEndStatus: SessionRunEndStatus | undefined =
			historical?.status === 'paused'
				? 'paused'
				: historical?.status === 'completed'
					? 'completed'
					: historical && isErrorStatus(historical.status)
						? 'error'
						: undefined;
		if (historical && historicalRunEndStatus) {
			dispatchSession({
				type: 'session/run-ended',
				sessionId,
				status: historicalRunEndStatus,
				reason:
					historical.run_end_reason ||
					sessionReducer.getSessionErrorReason(sessionId) ||
					(historicalRunEndStatus === 'error'
						? '本次会话因错误停止，暂未收到更具体的原因。'
						: ''),
			});
		}
	}

	function dismissAsk(messageId: string) {
		if (!messageId) return;
		dismissedAskIds = new Set(dismissedAskIds).add(messageId);
	}

	async function openPendingInteraction(id: string) {
		const request = interactionDict[id];
		if (!request || request.status !== 'pending') return;
		if (request.kind !== 'ask') {
			requestConfirmationOpen(request.id);
			return;
		}
		if (request.owner.kind !== 'session') return;
		if (request.sessionId !== activeSessionId) await switchToSession(request.owner.sessionId);
		if (sessionReducer.snapshot().activeSessionId !== request.owner.sessionId) return;
		dismissedAskIds = new Set([...dismissedAskIds].filter((dismissedId) => dismissedId !== id));
		chatViewController.setAutoFollow(false);
		await tick();
		const askCard = Array.from(
			messagesEl?.querySelectorAll('[data-interaction-id]') || [],
		).find((element) => element.getAttribute('data-interaction-id') === id);
		if (askCard instanceof HTMLElement)
			askCard.scrollIntoView?.({ behavior: 'smooth', block: 'center' });
	}

	function endSession() {
		return chatSessionController.endSession();
	}

	function requestDeleteSession() {
		if (!activeSessionId) return;
		deleteTarget = { sessionId: activeSessionId, title: sessionHeaderTitle };
	}

	async function confirmDeleteSession() {
		const target = deleteTarget;
		if (!target || deletingSession) return;
		deletingSession = true;
		try {
			await deleteSession({ sessionId: target.sessionId });
			addNotification('会话已删除', 'success', 2000);
		} catch (error) {
			reportError(error, { context: '+page', message: '删除失败', log: false });
		} finally {
			deleteTarget = null;
			deletingSession = false;
		}
	}

	function interruptOutput() {
		return chatSessionController.interruptOutput();
	}

	function handleContinue() {
		return chatSessionController.handleContinue();
	}

	// True when every currently awaiting ask card has at least one selected
	// option — InputRouter then allows Enter with an empty draft.
	let askSelectionsReady = $state(false);
	const askInteraction = createAskInteractionController({
		getActiveSessionId: () => activeSessionId,
		setAutoFollow: () => {
			chatViewController.setAutoFollow(true);
		},
		setSelectionsReady: (ready) => {
			askSelectionsReady = ready;
		},
		submitMessage: (text, images, files) => {
			void submitMessage(text, images, files);
		},
		reducer: sessionReducer,
	});
	const {
		clearAskAwaiting,
		computeAskSelectionsReady,
		handleInputSubmit: routeInputSubmission,
		handleAskSelectionChange,
		getAskSelection,
		handleAskSubmit,
		handleIgnoreAsk,
	} = askInteraction;

	$effect(() => {
		activeSessionId;
		askSelectionsReady = computeAskSelectionsReady();
	});

	// Entry point for the InputRouter component: it normalizes every input
	// format (typed text, pasted/picked images, attached files, voice) into a
	// single payload and forwards it here. The router already cleared its
	// draft, so the page just delivers the message and resumes auto-follow.
	// When every pending ask has selected options, Enter composes those
	// answers (space-joined) and appends any typed text; otherwise a typed
	// message bypasses the ask batch and resumes immediately.
	function handleInputSubmit({
		text,
		images,
		files,
	}: {
		text: string;
		images: ChatImageAttachment[];
		files: ChatFileAttachment[];
	}) {
		routeInputSubmission({ text, images, files });
	}

	function sessionStatusLabel(session: SessionSummary) {
		if (isErrorStatus(session.status)) return '错误';
		if (session.status === 'completed') return '空闲';
		const waitingLabel = waitingReasonLabel(sessionWaitingReason(session));
		if (waitingLabel) return waitingLabel;
		if (isPausedStatus(session.status)) return '已暂停';
		if (session.status === 'pending') return '排队中';
		if (session.status === 'running') return '运行中';
		return '空闲';
	}

	const activeSession = $derived(
		activeSessionId ? sessions.find((session) => session.id === activeSessionId) : null,
	);
	const sessionHeaderTitle = $derived(
		String(activeSession?.title || activeSession?.input || '新会话'),
	);
	const activeSessionStatusLabel = $derived(
		activeSession ? sessionStatusLabel(activeSession) : '空闲',
	);
	$effect(() => {
		activeSessionStatusLabelStore.set(activeSessionStatusLabel);
	});
</script>

<div class="chat-page responsive-layout-transition" bind:this={chatPageEl}>
	<RollbackDialog
		open={rollbackDialog.open}
		stepNumber={rollbackDialog.stepNumber}
		isUserMessage={rollbackDialog.role === 'user'}
		loading={rollbackLoading}
		onConfirm={confirmRollbackAction}
		onClose={() => {
			if (!rollbackLoading)
				rollbackDialog = {
					open: false,
					stepNumber: null,
					role: null,
					content: '',
					msgId: '',
				};
		}}
	/>
	<MaterialDialog
		open={deleteTarget !== null}
		title="删除会话"
		onClose={() => {
			if (!deletingSession) deleteTarget = null;
		}}
	>
		{#snippet children()}
			<p class="delete-session-confirmation">
				确定删除「{deleteTarget?.title ||
					'未命名会话'}」？此会话及其消息记录将被永久删除，且无法恢复。
			</p>
		{/snippet}
		{#snippet footer()}
			<MaterialButton
				variant="text"
				label="取消"
				disabled={deletingSession}
				onclick={() => (deleteTarget = null)}
			/>
			<MaterialButton
				variant="danger"
				label={deletingSession ? '删除中…' : '删除'}
				disabled={deletingSession}
				onclick={confirmDeleteSession}
			/>
		{/snippet}
	</MaterialDialog>

	<div class="desktop-session-rail responsive-layout-panel">
		<SessionRail
			{sessions}
			{activeSessionId}
			statusLabel={sessionStatusLabel}
			onNew={newSession}
			onSelect={switchToSession}
		/>
	</div>

	<div class="session-column">
		<SessionHeader
			title={sessionHeaderTitle}
			hasSession={!!activeSessionId}
			canEndSession={activeSessionStatus !== 'completed'}
			onNew={newSession}
			onDelete={requestDeleteSession}
			onEnd={endSession}
		>
			{#snippet children()}
				<SessionToolbar
					{activeSessionId}
					{showSessionMenu}
					{sessionMenuOpen}
					{menuSessions}
					{sessionLineage}
					{sessionLineageLoading}
					{sessionLineageError}
					onToggleSessionMenu={toggleSessionMenu}
					onSwitchSession={switchToSession}
					{sessionStatusLabel}
					{tokenStats}
					{tokenUsageDetails}
					{tokenStatsHint}
					{buildTokenTooltip}
				/>
			{/snippet}
		</SessionHeader>

		<div class="messages-wrap">
			<div
				class="messages-area"
				bind:this={messagesEl}
				role="region"
				aria-label="会话消息"
				onscroll={chatViewController.onScroll}
				onpointerdown={chatViewController.cancelJumpToBottom}
				onwheel={chatViewController.cancelJumpToBottom}
				use:dragScroll={{ axis: 'y', preserveTextSelection: true }}
			>
				<SessionTimeline
					{messages}
					{sessionToolRuns}
					mediaPlans={activeSessionId ? mediaPlansBySession[activeSessionId] || [] : []}
					loading={initialLoading}
					{hotkeyBinding}
					{awaitingBackground}
					{awaitingBackgroundCount}
					runEndStatus={activeSessionRunEndNotice?.status || null}
					runEndReason={activeSessionRunEndNotice?.reason || ''}
					{showContinueButton}
					{continueDisabled}
					continueBusy={continuePending}
					onContextMenu={handleContextMenu}
					onAskSelectionChange={handleAskSelectionChange}
					{getAskSelection}
					onIgnore={handleIgnoreAsk}
					onAskSubmit={handleAskSubmit}
					onAskDismiss={dismissAsk}
					onContinue={handleContinue}
				/>
			</div>
			{#if !autoFollow && messages.length > 0}
				<div class="jump-bottom-anchor">
					<MaterialIconButton
						size="toolbar"
						variant="tonal"
						className="jump-bottom"
						label="返回底部"
						title="返回底部"
						icon="arrowDown"
						onclick={chatViewController.jumpToBottom}
					></MaterialIconButton>
				</div>
			{/if}
		</div>

		<Composer
			bind:this={inputRouterRef}
			{activeSessionId}
			{hotkeyBinding}
			{isGenerating}
			{sessionRunning}
			interrupting={interruptPending}
			{askAwaiting}
			{askHasOptions}
			allowEmptySubmit={askSelectionsReady}
			{...inputLimits}
			onsubmit={handleInputSubmit}
			onstop={interruptOutput}
		>
			{#snippet toolbarLeft()}
				<PendingInteractionsMenu
					items={pendingInteractionItems}
					onSelect={openPendingInteraction}
				/>
			{/snippet}
			{#snippet toolbarRight()}
				<ModelToolbar
					{modelMenuOpen}
					{currentModelName}
					{currentModelId}
					{modelOptions}
					onToggleMenu={() => (modelMenuOpen = !modelMenuOpen)}
					onModelSelect={modelOperations.selectModel}
					{effortOptions}
					{currentEffort}
					onEffortSelect={modelOperations.selectEffort}
					{webSearchSupported}
					{webSearchOptions}
					{currentWebSearch}
					onWebSearchSelect={modelOperations.selectWebSearch}
				/>
			{/snippet}
		</Composer>
	</div>
</div>

<style>
	.delete-session-confirmation {
		margin: 0;
		color: var(--md-sys-color-on-surface);
	}
	.chat-page {
		position: relative;
		display: grid;
		grid-template-columns: minmax(0, 0px) minmax(0, 1fr);
		grid-template-rows: minmax(0, 1fr);
		flex: 1;
		width: 100%;
		min-width: 0;
		min-height: 0;
	}
	.chat-page.responsive-layout-transition {
		transition:
			grid-template-columns var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			column-gap var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.desktop-session-rail {
		display: flex;
		grid-column: 1;
		grid-row: 1;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
		visibility: hidden;
		opacity: 0;
		transform: translateX(-8px);
		pointer-events: none;
		transition:
			opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			visibility 0s linear var(--md-sys-motion-duration-short);
	}
	.session-column {
		position: relative;
		display: flex;
		grid-column: 1 / -1;
		grid-row: 1;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
	}
	.messages-wrap {
		position: relative;
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		/* Assistant messages and tool cards share the same reading column. */
		max-width: var(--md-sys-chat-max-width);
		margin: 0 auto;
		width: 100%;
	}
	.messages-area {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		overflow-x: clip;
		overscroll-behavior-x: none;
		touch-action: pan-y;
		padding: var(--md-sys-space-lg) var(--md-sys-space-md)
			calc(var(--chat-composer-clearance, 0px) + var(--md-sys-space-sm));
	}
	:global(.messages-area.drag-scroll--active) {
		cursor: grabbing;
	}
	.jump-bottom-anchor {
		position: absolute;
		right: var(--md-sys-space-md);
		bottom: calc(var(--chat-composer-clearance, 0px) + var(--md-sys-space-sm));
		display: flex;
		z-index: 5;
		pointer-events: none;
	}
	:global(.jump-bottom) {
		pointer-events: auto;
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-2);
		transition: background var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	:global(.jump-bottom > svg) {
		transition: transform var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	:global(.jump-bottom:hover > svg) {
		transform: translateY(var(--md-sys-space-2xs));
	}

	@media screen and (min-width: 840px) {
		.chat-page {
			grid-template-columns: 272px minmax(0, 1fr);
			--md-sys-chat-max-width: 1080px;
		}
		.desktop-session-rail {
			grid-column: 1;
			grid-row: 1;
			display: flex;
			visibility: visible;
			opacity: 1;
			transform: translateX(0);
			pointer-events: auto;
			transition:
				opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
				transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
				visibility 0s linear 0s;
			min-width: 0;
			min-height: 0;
			border-right: 1px solid var(--md-sys-color-outline-variant);
		}
		:global(.tab-panel--entering) .desktop-session-rail {
			animation: responsive-layout-panel-in var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard) both;
		}
		:global(.tab-panel--leaving) .desktop-session-rail {
			animation: session-rail-exit var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard) both;
		}
		.session-column {
			grid-column: 2;
			grid-row: 1;
		}
		.messages-wrap {
			max-width: min(100%, var(--md-sys-chat-max-width));
		}
		.messages-area {
			padding: var(--md-sys-space-2xl) var(--md-sys-space-2xl)
				calc(var(--chat-composer-clearance, 0px) + var(--md-sys-space-xl));
		}
		:global(.chat-page .session-switch),
		:global(.chat-page .session-header__new) {
			display: none;
		}
	}

	@keyframes session-rail-exit {
		from {
			opacity: 1;
			transform: translateX(0);
		}
		to {
			opacity: 0;
			transform: translateX(-8px);
		}
	}
</style>
