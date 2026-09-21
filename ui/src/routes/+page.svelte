<script>
	import logger from '$lib/logger.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { formatError } from '$lib/formatError.ts';
	import { buildResumeMessages, isDisplayOnlyMessageId } from '$lib/resumeMessages.ts';
	import {
		pickContinueStrategy,
		shouldResubmitOriginalUser,
		shouldShowContinueButton,
	} from '$lib/continueSession.ts';
	import { isBusyStatus, isErrorStatus, isPausedStatus } from '$lib/sessionStatus.ts';
	import { processResultSessionId, submitTranscript } from '$lib/submit.ts';
	import { createChatAgentEventHandlers } from '$lib/chatAgentEventHandlers.ts';
	import { createAskInteractionController } from '$lib/chatAskInteraction.ts';
	import { createChatSessionEventHandlers } from '$lib/chatSessionEventHandlers.ts';
	import { createChatUsageEventHandlers } from '$lib/chatUsageEventHandlers.ts';
	import { createChatModelSync } from '$lib/chatModelSync.ts';
	import { createStreamEventAggregator } from '$lib/streamAggregator.ts';
	import { registerPerformanceMetricsProvider } from '$lib/performanceMetrics.ts';
	import { createSessionRefreshScheduler } from '$lib/sessionRefresh.ts';
	import {
		appSessionReducer,
		DRAFT_SESSION_ID,
		sessionStateStore,
		resumeInteractions,
	} from '$lib/sessionReducer.ts';
	import {
		buildTokenUsageDetails,
		buildTokenUsageTooltip,
	} from '$lib/sessionUsagePresentation.ts';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { get } from 'svelte/store';
	import { invoke } from '$lib/tauri.ts';
	import {
		agentEventListeners,
		appEventListeners,
		registerListeners,
		sessionEventListeners,
	} from '$lib/events.ts';
	import {
		activeConversationStatusStore,
		addNotification,
		resumeTargetStore,
		rememberSessionError,
		forgetSessionError,
		getSessionErrorReason,
		updateModelState,
		modelStateStore,
		refreshActions,
		actionStore,
		mediaPlanStore,
		NEW_ACTION_INTENT_KEY,
		newSessionIntentStore,
	} from '$lib/stores.ts';
	import { syncStore } from '$lib/syncStore.ts';
	import { dragScroll } from '$lib/dragScroll.ts';
	import {
		CHAT_SCROLL_SETTLED_THRESHOLD,
		chatBottomOverlayClearance,
		isChatNearBottom,
		shouldFollowChatScroll,
	} from '$lib/chatScroll.ts';
	import RollbackDialog from '$lib/RollbackDialog.svelte';
	import {
		closeContextMenu as closeGlobalContextMenu,
		openContextMenuAt,
	} from '$lib/contextMenu.ts';
	import SessionToolbar from '$lib/SessionToolbar.svelte';
	import ModelToolbar from '$lib/ModelToolbar.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import SessionHeader from '$lib/SessionHeader.svelte';
	import ConversationTimeline from '$lib/ConversationTimeline.svelte';
	import Composer from '$lib/Composer.svelte';

	let chatPageEl = /** @type {HTMLElement | null} */ ($state(null));
	let inputRouterRef = /** @type {any} */ ($state(null));

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
	let messages = /** @type {Array<any>} */ ($state([]));
	let initialLoading = $state(true);
	const sessionReducer = appSessionReducer;
	let sessionState = $state(sessionReducer.getState());

	/** @param {import('$lib/sessionReducer.ts').SessionAction} action */
	function dispatchSession(action) {
		sessionReducer.dispatch(action);
	}

	$effect(() => syncStore(sessionStateStore, (next) => (sessionState = next)));

	const sessions = $derived(sessionState.sessions);
	const activeSessionId = $derived(sessionState.activeSessionId);
	// Interaction requests are shared by the ask cards and confirmation modal.
	// The modal keeps only its current presentation id; pending requests remain
	// owned by SessionReducer so ask/confirm/scheduled-confirm cannot drift.
	const interactionDict = $derived(sessionState.interactions || {});
	const pendingInteractions = $derived(
		Object.values(interactionDict).filter((request) => request.status === 'pending'),
	);
	const pendingAskInteractions = $derived(
		pendingInteractions.filter((request) => request.kind === 'ask'),
	);
	const askAwaiting = $derived(pendingAskInteractions.length > 0);
	const askHasOptions = $derived(
		pendingAskInteractions.some((request) => request.options.length > 0),
	);
	let rollbackDialog = $state({
		open: false,
		stepNumber: null,
		role: '',
		content: '',
		msgId: '',
	});
	let rollbackLoading = $state(false);

	// Model switcher state: the registry catalog plus the current default
	// model name, displayed on the toolbar button and filtered in the menu.
	let modelMenuOpen = $state(false);
	let sessionMenuOpen = $state(false);
	let modelOptions = /** @type {Array<any>} */ ($state([]));
	let currentModelName = $state('');
	let currentModelId = $state('');
	let currentEffort = $state('');
	// Provider built-in web search mode ("off" | "auto" | "always").
	// Defaults to off (opt-in); "auto" lets the model decide when to search.
	let currentWebSearch = $state('off');
	/** Default-model provider wire style supports built-in 联网搜索. */
	let webSearchSupported = $state(false);
	/** Normalized wire style of the default-model provider (for mode filtering). */
	let currentApiStyle = $state('openai-chat');
	// The configured recording hotkey binding, loaded from settings and kept
	// in sync via `hotkey:rebind` so placeholders show the real value.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	// Active session token stats from the reducer. Cleared when the active
	// session changes; updated on every `agent:usage` event.
	/**
	 * @typedef {object} SessionTokenStats
	 * @property {number} promptTokens
	 * @property {number} completionTokens
	 * @property {number} totalTokens
	 * @property {number} [cachedTokens]
	 * @property {number} [cacheCreationTokens]
	 * @property {number} [cacheMissTokens]
	 * @property {number} [contextTokens]
	 * @property {boolean} [cacheExclusive]
	 * @property {number} cumulativePromptTokens
	 * @property {number} cumulativeCompletionTokens
	 * @property {number} cumulativeTotalTokens
	 * @property {number} [cumulativeCachedTokens]
	 * @property {number} [cumulativeCacheCreationTokens]
	 * @property {number|null} costUsd
	 * @property {number|null} cumulativeCostUsd
	 * @property {number|null} contextWindow
	 * @property {string|null} model
	 * @property {boolean} [restored] - entry came from persistence (resume /
	 *   reopened conversation) with no live `agent:usage` events expected;
	 *   the current context falls back to the latest persisted call.
	 */

	/** @type {SessionTokenStats | null} */
	let tokenStats = $state(null);
	$effect(() => {
		tokenStats = activeSessionId
			? /** @type {SessionTokenStats | undefined} */ (
					sessionState.tokenStats?.[activeSessionId]
				) || null
			: null;
	});
	// Clear per-session stats when the active session changes so a stale entry
	// from a previous session doesn't bleed into the new session's display.
	$effect(() => {
		const _ = activeSessionId;
		// Subscribe so any store change refreshes; the actual filter is in
		// the subscription above. This effect just guarantees an unsubscribed
		// session is wiped when the user starts a fresh conversation.
		if (!activeSessionId) tokenStats = null;
	});

	// Per-LLM-call usage detail for the active session (restored from the
	// persisted `llm_usage` when a resume conversation opens). Used by the
	// session-level token tooltip and call count.
	/** @type {Array<import('$lib/sessionUsage.ts').LlmUsage>} */
	let llmUsage = $state([]);
	$effect(() => {
		llmUsage = activeSessionId ? sessionState.llmUsage?.[activeSessionId] || [] : [];
	});
	$effect(() => {
		const _ = activeSessionId;
		if (!activeSessionId) llmUsage = [];
	});

	/** @param {any} stats */
	function buildTokenTooltip(stats) {
		return buildTokenUsageTooltip(stats, llmUsage);
	}

	// Send/interrupt merged button: text takes priority (always send); with no
	// text and the agent actively generating output the button interrupts the
	// current output while keeping the session resumable.
	let modelState = $state('ready');
	let interruptPending = $state(false);
	$effect(() =>
		syncStore(modelStateStore, (v) => {
			modelState = v;
		}),
	);
	const isGenerating = $derived(
		modelState === 'streaming' || modelState === 'tool' || modelState === 'stalled',
	);
	const sessionRunning = $derived(
		!!activeSessionId &&
			sessions.some((t) => t.id === activeSessionId && isBusyStatus(t.status)),
	);
	// Tooltip for the token widget. While the active session is still running
	// (streaming, tool-calling, or queued) more `agent:usage` events are
	// expected. A finished or history-opened conversation with no persisted
	// usage will never receive events, so show a neutral hint instead.
	const tokenStatsHint = $derived(isGenerating || sessionRunning ? '等待 LLM 统计' : '暂无统计');
	const tokenUsageDetails = $derived.by(() =>
		tokenStats ? buildTokenUsageDetails(tokenStats, llmUsage) : null,
	);
	// Menu source: parallel sessions plus paused ones — a paused session is
	// otherwise invisible in the chat view (its conversation is not shown).
	const menuSessions = $derived(
		sessions.filter((t) => isBusyStatus(t.status) || isPausedStatus(t.status)),
	);
	const showSessionMenu = $derived(menuSessions.length >= 2);

	// Live action registry (for "waiting on background" banner). Synced from
	// the global actionStore kept by +layout.
	let actionsById = $state(/** @type {Record<string, any>} */ ({}));
	$effect(() => syncStore(actionStore, (v) => (actionsById = v || {})));
	let mediaPlansBySession = $state(/** @type {Record<string, any[]>} */ ({}));
	$effect(() => syncStore(mediaPlanStore, (v) => (mediaPlansBySession = v || {})));
	const activeSessionStatus = $derived(
		activeSessionId ? sessions.find((t) => t.id === activeSessionId)?.status : undefined,
	);
	/** Plain paused (not ask/confirm) with still-running background actions for this session. */
	const awaitingBackground = $derived.by(() => {
		if (!activeSessionId || activeSessionStatus !== 'paused') return false;
		return Object.values(actionsById).some(
			(a) =>
				a &&
				a.kind !== 'scheduled' &&
				a.status === 'running' &&
				a.sessionId === activeSessionId,
		);
	});
	const awaitingBackgroundCount = $derived.by(() => {
		if (!activeSessionId || activeSessionStatus !== 'paused') return 0;
		return Object.values(actionsById).filter(
			(a) =>
				a &&
				a.kind !== 'scheduled' &&
				a.status === 'running' &&
				a.sessionId === activeSessionId,
		).length;
	});

	const effortOptions = [
		{ value: '', label: '默认' },
		{ value: 'low', label: '低' },
		{ value: 'medium', label: '中' },
		{ value: 'high', label: '高' },
		// Vendor thinking off (DeepSeek/Kimi/Responses); omitted for plain OpenAI.
		{ value: 'off', label: '关闭' },
	];

	const webSearchOptionsAll = [
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

	/** @param {string} value */
	async function handleWebSearchSelect(value) {
		if (!webSearchSupported && value !== 'off') {
			addNotification('当前模型线协议不支持内置联网搜索', 'info', 3000);
			return;
		}
		const label = webSearchOptionsAll.find((o) => o.value === value)?.label || '关闭';
		skipNextDefaultModelRefresh = true;
		try {
			await invoke('set_web_search', { role: 'default_model', mode: value });
			currentWebSearch = value;
			addNotification(`联网搜索: ${label}`, 'success', 2500);
		} catch (e) {
			skipNextDefaultModelRefresh = false;
			reportError(e, { context: '+page', message: '设置联网搜索失败', log: false });
		}
	}

	/** @param {any} m */
	async function handleModelSelect(m) {
		modelMenuOpen = false;
		skipNextDefaultModelRefresh = true;
		try {
			await invoke('switch_model', { role: 'default_model', modelId: m.id });
			currentModelId = m.id;
			currentModelName = m.name || m.id;
			addNotification(`已切换默认模型: ${currentModelName}`, 'success', 3000);
		} catch (e) {
			skipNextDefaultModelRefresh = false;
			reportError(e, { context: '+page', message: '切换模型失败', log: false });
		}
	}

	/** @param {string} value */
	async function handleEffortSelect(value) {
		const label = effortOptions.find((o) => o.value === value)?.label || '默认';
		skipNextDefaultModelRefresh = true;
		try {
			await invoke('set_reasoning_effort', { role: 'default_model', effort: value || null });
			currentEffort = value || '';
			addNotification(`思考强度: ${label}`, 'success', 2500);
		} catch (e) {
			skipNextDefaultModelRefresh = false;
			reportError(e, { context: '+page', message: '设置思考强度失败', log: false });
		}
	}

	// Right-click context menu state
	let ctxMenu = $state({
		stepNumber: null,
		content: '',
		role: '',
		msgId: '',
		selectedContent: '',
	});

	/** @param {any} ev */
	function handleContextMenu(ev) {
		const next = {
			stepNumber: ev.stepNumber,
			content: ev.content,
			role: ev.role,
			msgId: ev.messageId,
			selectedContent: ev.selectedContent || '',
		};
		ctxMenu = next;
		openContextMenuAt(ev.x, ev.y, [
			...(isDisplayOnlyMessageId(next.msgId)
				? []
				: [
						{
							id: 'rollback',
							label: '回退到此消息',
							icon: 'rollback',
							action: handleCtxRollback,
						},
					]),
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
		if (isDisplayOnlyMessageId(ctxMenu.msgId)) {
			closeCtxMenu();
			return;
		}
		const step = getStepForCtxMenu();
		if (step == null) {
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
				logger.warn('+page', 'context menu copy failed', formatError(error));
				addNotification('复制失败', 'error', 2000);
			}
		}
		closeCtxMenu();
	}

	function closeCtxMenu() {
		ctxMenu = {
			stepNumber: null,
			content: '',
			role: '',
			msgId: '',
			selectedContent: '',
		};
		closeGlobalContextMenu();
	}

	/** @param {MouseEvent} e */
	function handleWindowClick(e) {
		if (modelMenuOpen) {
			const menu = document.querySelector('.model-menu');
			const btn = document.querySelector('.model-switch-btn');
			if (
				menu &&
				btn &&
				!menu.contains(/** @type {Node} */ (e.target)) &&
				!btn.contains(/** @type {Node} */ (e.target))
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
				!menu.contains(/** @type {Node} */ (e.target)) &&
				!btn.contains(/** @type {Node} */ (e.target))
			) {
				sessionMenuOpen = false;
			}
		}
	}

	$effect(() => {
		if (sessionMenuOpen && menuSessions.length < 2) sessionMenuOpen = false;
	});

	// Merged into existing onMount/onDestroy below

	async function confirmRollbackAction() {
		const { stepNumber, role, content, msgId } = rollbackDialog;
		const rollbackSessionId = activeSessionId;
		if (!rollbackSessionId) return;
		rollbackLoading = true;
		try {
			if (role === 'user') {
				if (!/^msg-[0-9a-f]{32}$/.test(msgId)) {
					addNotification('消息仍在保存，请稍后再试', 'info', 2000);
					return;
				}
				// User-message rollback: pause the session and put the message
				// text back in the input box so the user can edit and re-send.
				await invoke('rollback_session', {
					sessionId: rollbackSessionId,
					targetStep: stepNumber,
					pause: true,
					targetMessageId: msgId,
				});
				dispatchSession({ type: 'session/replay-reset', sessionId: rollbackSessionId });
				clearStepBlockIds(rollbackSessionId);
				// The backend is the source of truth for what the rollback
				// deleted (target message + its whole discarded timeline);
				// rebuild from the DB instead.
				await resyncSessionMessages(rollbackSessionId);
				inputRouterRef?.setDraft(content);
				addNotification('已回退，请编辑后重新发送', 'info', 3000);
			} else {
				await invoke('rollback_session', {
					sessionId: rollbackSessionId,
					targetStep: stepNumber,
					pause: false,
					targetMessageId: msgId,
				});
				dispatchSession({ type: 'session/replay-reset', sessionId: rollbackSessionId });
				clearStepBlockIds(rollbackSessionId);
				await resyncSessionMessages(rollbackSessionId);
				addNotification(`已回退到第 ${stepNumber} 步`, 'info', 3000);
			}
		} catch (e) {
			reportError(e, { context: '+page', message: '回退失败', log: false });
		}
		rollbackLoading = false;
		rollbackDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' };
		await loadSessions();
	}

	// Rebuild a session's in-memory message list from the authoritative DB
	// state. Used after rollback (and by handleContinue) so the UI cannot
	// diverge from what the backend actually kept/deleted.
	/** @param {string} sessionId */
	function pendingInteractionIdsForSession(sessionId) {
		return Object.values(sessionReducer.getState().interactions || {})
			.filter((request) => request.sessionId === sessionId && request.status === 'pending')
			.map((request) => request.id);
	}

	/** @param {string | null} sessionId */
	async function resyncSessionMessages(sessionId) {
		if (!sessionId) return;
		try {
			const result = await invoke('get_session_for_resume', { sessionId });
			dispatchSession({
				type: 'session/messages/resume-loaded',
				sessionId,
				messages: buildResumeMessages(result),
				interactions: resumeInteractions(result),
				preserveInteractionIds: pendingInteractionIdsForSession(sessionId),
				usage: result.usage,
				llmUsage: result.llm_usage,
				preserveStreamingOnly: true,
			});
		} catch (e) {
			reportError(e, { context: '+page', message: '同步消息失败', log: false });
		}
	}

	function newSession() {
		if (activeSessionId) {
			dispatchSession({ type: 'session/memory-cleared', sessionId: activeSessionId });
		}
		// 新对话 = explicit fresh start. While `newSessionIntentStore` is set, no
		// event-driven path may auto-assign an existing session (loadSessions
		// auto-assign, session:created, auto-restore), otherwise the next message
		// would append to the old conversation instead of starting a new session.
		// The intent is cleared only when the user's own submission creates a
		// session (submit.ts) or they explicitly switch to another session. Also
		// persisted to localStorage so the next app launch skips restoring the
		// previous conversation.
		newSessionIntentStore.set(true);
		if (browser) localStorage.setItem(NEW_ACTION_INTENT_KEY, '1');
		dispatchSession({ type: 'session/cleared' });
		sessionMenuOpen = false;
	}

	// Switch the chat view to another parallel session. Merges the persisted
	// DB messages with any in-memory streaming messages that arrived
	// concurrently (the session may still be running).
	// A terminal session has no more streaming events: drop its in-memory
	// message list, token stats and seq bookkeeping (switchToSession reloads
	// everything from the DB on demand). Keeps parallel-conversation memory
	// bounded across a long session. Never evicts the active conversation.
	/** @param {string | null} sessionId */
	function evictTerminalSessionMemory(sessionId) {
		if (!sessionId || (activeSessionId && sessionId === activeSessionId)) return;
		dispatchSession({ type: 'session/memory-cleared', sessionId });
	}

	/** @param {string} sessionId */
	async function switchToSession(sessionId) {
		sessionMenuOpen = false;
		// The previously active session is about to be deactivated: if it is
		// already terminal (completed/error — it never evicted while it was
		// being watched), reclaim its memory after the switch (evicting BEFORE
		// it would be skipped by evictTerminalSessionMemory's active guard);
		// switchToSession reloads from the DB when it is re-opened.
		const prevActive = activeSessionId;
		try {
			const result = await invoke('get_session_for_resume', { sessionId });
			// Live tool cards and DB step badges share the same `step-*` id
			// (minted by the backend when the action started), so the merge
			// dedups them by id alone — a mid-step card keeps streaming its
			// observation, the DB copy wins once it is finalized.
			dispatchSession({
				type: 'session/messages/resume-loaded',
				sessionId,
				messages: buildResumeMessages(result),
				interactions: resumeInteractions(result),
				preserveInteractionIds: pendingInteractionIdsForSession(sessionId),
				usage: result.usage,
				llmUsage: result.llm_usage,
			});
			// An explicit switch abandons the fresh-start intent: the chosen
			// session becomes the active conversation (and may be auto-restored
			// on the next app launch).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			dispatchSession({ type: 'session/selected', sessionId });
			// Reclaim the deactivated session's memory when it is terminal.
			// `get_sessions` lists only the executor's in-memory working set
			// and terminal sessions are REMOVED from it, so a completed/errored
			// session is never found by the status check — evict on the
			// "missing from the list" branch too.
			if (prevActive && prevActive !== sessionId) {
				const prevSession = sessions.find((x) => x.id === prevActive);
				if (
					!prevSession ||
					prevSession.status === 'completed' ||
					isErrorStatus(prevSession.status)
				) {
					evictTerminalSessionMemory(prevActive);
				}
			}
			const t = sessions.find((x) => x.id === sessionId);
			addNotification(`已切换到：${t?.title || '会话'}`, 'info', 1500);
		} catch (e) {
			reportError(e, { context: '+page', message: '切换会话失败', log: false });
		}
	}

	async function endSession() {
		if (!activeSessionId) return;
		// While the end is in flight, no event may resurrect the ended session.
		newSessionIntentStore.set(true);
		const endedId = activeSessionId;
		try {
			await invoke('end_session', { sessionId: endedId });
		} catch (e) {
			// The session is still alive server-side: keep the view attached to
			// it so the user can retry. Clearing the pointer here would orphan a
			// session that keeps running (and streaming) with no visible target.
			newSessionIntentStore.set(false);
			reportError(e, { context: '+page', message: '完成会话失败', log: false });
			return;
		}
		// Keep the finished conversation selected so its terminal reason remains
		// visible in the timeline. The fresh-start intent makes the next message
		// create a new session; the user can also use the new-session button.
	}

	async function interruptOutput() {
		if (!activeSessionId || interruptPending) return;
		interruptPending = true;
		try {
			await invoke('interrupt_session', { sessionId: activeSessionId });
			addNotification('输出已中断，可继续生成', 'info', 2000);
		} catch (e) {
			reportError(e, { context: '+page', message: '中断输出失败', log: false });
		} finally {
			interruptPending = false;
		}
	}

	async function handleContinue() {
		if (!activeSessionId || continuePending) return;
		continuePending = true;
		const tid = activeSessionId;
		const currentMessages = sessionReducer.getMessages(tid);
		// A retry can begin as soon as continue_session resolves. Keep only
		// bubbles created after this point when merging its DB snapshot: every
		// pre-existing bubble is either represented by the DB or was explicitly
		// removed there as a failed-stream partial. Classifying a whole trailing
		// assistant suffix as partial erased completed tool rounds after errors.
		const preContinueMessageIds = new Set(currentMessages.map((m) => m.id));
		// Strategy must be picked before truncate: mid-generation partials are
		// what distinguish "send 继续" from "pass the original user message".
		const strategy = pickContinueStrategy(currentMessages);
		try {
			// First unblock the errored session: continue_session truncates the
			// partial output and sets the session to Pending so a follow-up user
			// message below is accepted instead of being dropped as a
			// terminal-state supplement.
			await invoke('continue_session', { sessionId: tid });
			dispatchSession({ type: 'session/error-cleared', sessionId: tid });
			// Re-sync from the authoritative post-continue DB state. Any retry
			// stream that won the race with this request has a fresh id and is
			// retained; stale pre-continue UI entries cannot leak back in.
			try {
				const result = await invoke('get_session_for_resume', { sessionId: tid });
				dispatchSession({
					type: 'session/messages/resume-loaded',
					sessionId: tid,
					messages: buildResumeMessages(result),
					interactions: resumeInteractions(result),
					preserveInteractionIds: pendingInteractionIdsForSession(tid),
					usage: result.usage,
					llmUsage: result.llm_usage,
					preserveStreamingOnly: true,
					excludeMessageIds: [...preContinueMessageIds],
				});
			} catch (e) {
				// Keep the current view until a later sync succeeds. A failed read
				// is not evidence that any visible history is a failed partial.
			}
			dispatchSession({ type: 'session/replay-reset', sessionId: tid });
			// Two strategies:
			// - LLM mid-generation interrupt → send "继续" as a real user turn.
			// - User message sent but agent never generated → pass the original
			//   text (resubmit only when it did not survive as a persisted
			//   trailing user turn; otherwise Pending resume alone retries).
			autoFollow = true;
			if (strategy.mode === 'continue') {
				submitMessage(strategy.text, []);
			} else {
				const synced = sessionReducer.getMessages(tid);
				if (shouldResubmitOriginalUser(synced, strategy.text)) {
					submitMessage(strategy.text, []);
				}
			}
			await loadSessions();
		} catch (e) {
			reportError(e, { context: '+page', message: '继续失败', log: false });
			// Keep the banner visible so the user can retry.
		} finally {
			continuePending = false;
		}
	}

	// Tauri event listener handle (registered in onMount, disposed in
	// onDestroy). See eventRegistrations below.
	let eventRegistrations = /** @type {{ ready: Promise<void>; dispose: () => void } | null} */ (
		null
	);
	let messagesEl = /** @type {HTMLElement | null | undefined} */ (undefined);
	let autoFollow = $state(true);
	let scrollRafPending = false;
	let jumpingToBottom = false;
	let jumpBottomTimer = /** @type {ReturnType<typeof setTimeout> | null} */ (null);
	let dead = false;
	// Guards concurrent loadSessions() calls so a stale response can't overwrite
	// a newer one.
	let loadSessionsSeq = 0;

	// Visible messages are a projection of the reducer. Interaction metadata is
	// joined by the stable request/message id, never by text or position.
	$effect(() => {
		const list = sessionState.messages?.[activeSessionId || DRAFT_SESSION_ID] || [];
		const interactions = sessionState.interactions || {};
		messages = list.map((message) => {
			if (message.type !== 'ask') return message;
			const request = interactions[message.id];
			if (!request || request.kind !== 'ask') return message;
			const response = /** @type {{ answer?: string; ignored?: boolean } | undefined} */ (
				request.response
			);
			return {
				...message,
				options: request.options,
				awaiting: request.status === 'pending',
				resolved:
					request.status === 'resolved'
						? response?.ignored
							? { ignored: true }
							: { answer: response?.answer || '' }
						: null,
			};
		});
	});

	const activeSessionError = $derived(
		!!activeSessionId && sessionState.error?.sessionId === activeSessionId,
	);
	const activeSessionTermination = $derived(
		!!activeSessionId && sessionState.termination?.sessionId === activeSessionId
			? sessionState.termination
			: null,
	);
	const sessionErrorId = $derived(sessionState.error?.sessionId || null);
	const sessionErrorReason = $derived(sessionState.error?.reason || '');
	let continuePending = $state(false);
	const showContinueButton = $derived(
		!!activeSessionId && shouldShowContinueButton(messages, activeSessionError),
	);
	// Keep the affordance visible for every user-tail conversation, but do not
	// let it race a normal pending/running turn. `continue_session` is only a
	// retry operation for paused/error sessions.
	const continueDisabled = $derived(
		continuePending || (!activeSessionError && !isPausedStatus(activeSessionStatus)),
	);

	// Clear error state when the active session changes.
	$effect(() => {
		const _ = activeSessionId;
		if (sessionErrorId && activeSessionId !== sessionErrorId) {
			dispatchSession({ type: 'session/error-cleared', sessionId: sessionErrorId });
		}
	});

	// Auto-scroll to the newest message whenever messages change.
	$effect(() => {
		const _ = messages;
		if (messages.length > 0) {
			scrollToBottom();
		}
	});

	// When the active session changes (e.g. switching to a reviewed session or
	// creating a new session), re-enable follow and scroll to the bottom.
	$effect(() => {
		const _ = activeSessionId;
		autoFollow = true;
		scrollToBottom();
	});

	function scrollToBottom() {
		if (!messagesEl || dead || scrollRafPending) return;
		scrollRafPending = true;
		requestAnimationFrame(() => {
			scrollRafPending = false;
			// Re-check autoFollow here so a user scroll-up between the call
			// and the rAF callback is respected (not overridden).
			if (dead || !messagesEl || !autoFollow) return;
			messagesEl.scrollTop = messagesEl.scrollHeight;
		});
	}

	// Cold-mount scroll for conversations opened as a bulk snapshot (history
	// resume, app-start auto-restore): at that moment every bubble is
	// content-visibility-skipped and reports only its contain-intrinsic-size
	// estimate (~120px), so the first scrollToBottom lands above the real
	// bottom. Force one full render pass — the real sizes are then remembered
	// by `contain-intrinsic-size: auto` — scroll, and restore lazy rendering.
	function scrollToBottomAfterOpen() {
		if (dead || !messagesEl) return;
		const list = messagesEl.querySelector('.message-list');
		if (!list) return;
		const bubbles = /** @type {NodeListOf<HTMLElement>} */ (list.querySelectorAll('.bubble'));
		bubbles.forEach((b) => b.style.setProperty('content-visibility', 'visible'));
		messagesEl.scrollTop = messagesEl.scrollHeight;
		let frames = 2;
		const finish = () => {
			frames -= 1;
			if (frames > 0) {
				requestAnimationFrame(finish);
				return;
			}
			if (dead || !messagesEl) return;
			if (autoFollow) messagesEl.scrollTop = messagesEl.scrollHeight;
			bubbles.forEach((b) => b.style.removeProperty('content-visibility'));
		};
		requestAnimationFrame(finish);
	}

	function onScroll() {
		if (!messagesEl) return;
		const settledAtBottom = isChatNearBottom(messagesEl, CHAT_SCROLL_SETTLED_THRESHOLD);
		// Keep the button hidden while the requested smooth scroll is settling.
		// Otherwise each intermediate scroll event briefly marks the view as
		// detached and makes the button flicker back in.
		if (jumpingToBottom) {
			if (settledAtBottom) stopJumpToBottom();
			return;
		}
		autoFollow = shouldFollowChatScroll(messagesEl, autoFollow);
	}

	function stopJumpToBottom() {
		jumpingToBottom = false;
		if (jumpBottomTimer) {
			clearTimeout(jumpBottomTimer);
			jumpBottomTimer = null;
		}
	}

	function cancelJumpToBottom() {
		if (!jumpingToBottom) return;
		stopJumpToBottom();
		if (messagesEl) autoFollow = isChatNearBottom(messagesEl);
	}

	function jumpToBottom() {
		if (!messagesEl) return;
		stopJumpToBottom();
		autoFollow = true;
		jumpingToBottom = true;
		messagesEl.scrollTo({ top: messagesEl.scrollHeight, behavior: 'smooth' });
		// WebViews normally emit a final scroll event, but the timeout also
		// releases the guard if the target is already at the end or events are
		// coalesced by the platform.
		jumpBottomTimer = setTimeout(() => {
			stopJumpToBottom();
			if (messagesEl) autoFollow = isChatNearBottom(messagesEl);
		}, 700);
	}

	const streamEvents = createStreamEventAggregator({
		getActiveSessionId: () => activeSessionId,
		onActiveStream: () => updateModelState('streaming'),
		dispatch: dispatchSession,
		getBlockIds: (sessionId, stepNumber, runId) =>
			sessionReducer.getBlockIds(sessionId, stepNumber, runId),
	});
	const { chunkHandler, clearStepBlockIds, flushChunksNow, metricsSnapshot } = streamEvents;
	const unregisterPerformanceMetricsProvider =
		registerPerformanceMetricsProvider(metricsSnapshot);

	// Model discovery and default-model settings synchronization live outside the
	// route component; this page only supplies Svelte state setters.
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

	// Open a reviewed conversation (from the history page). The chat view
	// stays mounted while other tabs are open, so this runs both at mount and
	// whenever the store changes afterwards.
	/** @param {any} resumeTarget */
	function retainErroredSession(resumeTarget) {
		if (!resumeTarget?.sessionId) return;
		dispatchSession({
			type: 'session/retained-error',
			session: {
				id: resumeTarget.sessionId,
				input: resumeTarget.summary || '',
				input_text: resumeTarget.summary || '',
				title: resumeTarget.title || null,
				status: 'error',
			},
		});
	}

	/** @param {any} resumeTarget */
	function processResumeTarget(resumeTarget) {
		if (resumeTarget && resumeTarget.sessionId) {
			// Opening a reviewed conversation abandons any pending fresh-start
			// intent (the user chose this conversation explicitly).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			const prevActive = activeSessionId;
			dispatchSession({ type: 'session/selected', sessionId: resumeTarget.sessionId });
			// The session being left: if it is terminal (or has dropped out of
			// the executor's working set, which only happens for terminal
			// sessions), reclaim its in-memory messages/token stats — they are
			// reloaded from the DB if the user returns. Runs AFTER the switch
			// because evictTerminalSessionMemory skips the active session.
			// Same rule as switchToSession; without it every reviewed session
			// would keep its full message list in memory for the whole app run.
			if (prevActive && prevActive !== resumeTarget.sessionId) {
				const prevSession = sessions.find((x) => x.id === prevActive);
				if (
					!prevSession ||
					prevSession.status === 'completed' ||
					prevSession.status === 'error'
				) {
					evictTerminalSessionMemory(prevActive);
				}
			}
			// Opening an errored session is read-only. Preserve the error state and
			// show the reason instead of silently converting it to Paused.
			if (resumeTarget.wasError) {
				dispatchSession({
					type: 'session/error-shown',
					sessionId: resumeTarget.sessionId,
					reason:
						resumeTarget.errorReason ||
						getSessionErrorReason(resumeTarget.sessionId) ||
						'本次会话因错误停止，暂未收到更具体的原因。',
				});
				retainErroredSession(resumeTarget);
			}
			// Defer clearing so it survives rapid remounts during init.
			setTimeout(() => resumeTargetStore.set(null), 0);
		}
	}

	// The resume target set by the history page's "open session" flow must be
	// handled while the chat view is already mounted. `$effect` does NOT track
	// `get(store)` (svelte/store wraps the read in `untrack`), so a plain
	// `get(resumeTargetStore)` here would only see the initial value and never
	// react to later history clicks. Subscribing via syncStore runs the callback
	// on every store change (and synchronously once with the current value).
	$effect(() => syncStore(resumeTargetStore, (v) => processResumeTarget(v)));

	// The composer is a bottom overlay so messages can continue underneath its
	// transparent outer area. Measure the actual distance from the page bottom to
	// the composer's top instead of deriving it from height + a duplicated CSS
	// offset. This keeps the last message above the opaque inner surface even
	// after a resize, attachment change, or narrow-window reflow.
	$effect(() => {
		const page = chatPageEl;
		if (!browser || !page || typeof ResizeObserver === 'undefined') return;
		const composer = page.querySelector('.input-area');
		if (!(composer instanceof HTMLElement)) return;

		const updateComposerClearance = () => {
			const pageRect = page.getBoundingClientRect();
			const composerRect = composer.getBoundingClientRect();
			const clearance = chatBottomOverlayClearance(pageRect.bottom, composerRect.top);
			page.style.setProperty('--chat-composer-clearance', `${clearance}px`);
			// A composer resize changes scrollHeight via the padding below. Preserve
			// the user's follow-to-bottom intent after that layout update.
			if (autoFollow) scrollToBottom();
		};
		const observer = new ResizeObserver(updateComposerClearance);
		observer.observe(composer);
		observer.observe(page);
		updateComposerClearance();
		return () => {
			observer.disconnect();
			page.style.removeProperty('--chat-composer-clearance');
		};
	});

	onMount(async () => {
		// Hydrate the fresh-start intent from localStorage BEFORE any data
		// load: the store is in-memory only, but the intent survives app
		// restarts via `haven.no_auto_restore`. Without this, `loadSessions`
		// auto-assign would re-select the old conversation on restart and the
		// persisted intent would be silently defeated. The resumeTarget
		// branch below (an explicit user choice) clears it again if needed.
		if (browser && localStorage.getItem(NEW_ACTION_INTENT_KEY)) {
			newSessionIntentStore.set(true);
		}

		// Process resume target first so loadSessions won't overwrite
		// activeSessionId with a stale paused session whose messages are gone.
		const initialResumeTarget = get(resumeTargetStore);
		processResumeTarget(initialResumeTarget);

		// Register listeners BEFORE any async data load so session/streaming
		// events arriving while the page initializes are never missed.
		const registrations = registerListeners(
			{
				...sessionEventListeners(
					createChatSessionEventHandlers({
						getActiveSessionId: () => activeSessionId,
						isFreshSessionIntent: () => get(newSessionIntentStore),
						adoptDraftMessages: (sessionId) => {
							const adopted = sessionReducer.getMessages(DRAFT_SESSION_ID).length > 0;
							dispatchSession({ type: 'session/messages/adopt-draft', sessionId });
							return adopted;
						},
						dispatchSession,
						getSessionErrorId: () => sessionErrorId,
						rememberSessionError,
						forgetSessionError,
						clearAskAwaiting: (sessionId) => {
							clearAskAwaiting(sessionId);
							dispatchSession({ type: 'session/interactions-cleared', sessionId });
						},
						evictTerminalSessionMemory,
						clearStepBlockIds,
						flushChunksNow,
						updateSessionTitle: (sessionId, title) => {
							const index = sessions.findIndex((session) => session.id === sessionId);
							if (index >= 0) {
								dispatchSession({
									type: 'session/title-updated',
									sessionId,
									title,
								});
								// Keep the shell's task/status view in sync with the chat
								// header as soon as the generated title arrives.
							} else {
								// A title event can win the race with the initial session
								// list load. The persisted title will be picked up here.
								scheduleLoadSessions();
							}
						},
						scheduleLoadSessions,
					}),
				),
				...appEventListeners({
					'hotkey:rebind': (event) => {
						const data = event.payload;
						if (data.newBinding) {
							hotkeyBinding = data.newBinding;
						}
					},
					// Settings save / model switch rebuilds the router. Keep-alive
					// leaves this page mounted, so re-pull the default_model role
					// instead of leaving the toolbar on a stale selection.
					'llm:config_changed': () => {
						if (skipNextDefaultModelRefresh) {
							skipNextDefaultModelRefresh = false;
							return;
						}
						refreshDefaultModelFromBackend();
					},
				}),
				...agentEventListeners(
					createChatAgentEventHandlers({
						chunkHandler,
						flushChunksNow,
						dispatchSession,
					}),
				),
				...agentEventListeners(createChatUsageEventHandlers({ dispatchSession })),
			},
			{ tag: '+page' },
		);
		eventRegistrations = registrations;
		const readyP = registrations.ready;
		// Tauri listener registration is asynchronous. Complete it before any
		// restore/reopen call can trigger a confirmation, otherwise the event can
		// be emitted into the small registration gap and the modal never appears.
		await readyP;
		if (dead) return;

		// Load the current default model for the toolbar model switcher and
		// populate the menu with models discovered from the default provider's
		// `/models` endpoint, mirroring the settings page behavior. Empty
		// api_key falls back to the stored key via the role name, and
		// discovery is skipped when no base URL is set. Fire-and-forget so it
		// never delays the conversation render.
		invoke('get_settings')
			.then((s) => {
				// The default_model role references a provider + a model id on
				// that provider (the "model library" is the provider's fetched
				// model list). Resolve both for the toolbar switcher.
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
				logger.warn('+page', 'get_settings error', e);
			});

		// Load the session list and auto-restore the last conversation in
		// parallel; the conversation renders as soon as its data arrives,
		// without waiting for `reopen_session` (a second IPC round-trip that
		// only makes the session resumable for follow-up messages).
		const sessionsP = loadSessions();
		const restoreP = restoreLastConversation(initialResumeTarget);

		try {
			await Promise.all([sessionsP, restoreP]);
		} finally {
			initialLoading = false;
		}

		// Conversation just opened (history resume or auto-restore): scroll to
		// the real bottom, forcing the estimated content-visibility heights to
		// render first (see scrollToBottomAfterOpen).
		if (activeSessionId) {
			await tick();
			scrollToBottomAfterOpen();
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
		eventRegistrations?.dispose();
		loadSessionsRefresh.dispose();
		stopJumpToBottom();
		if (browser) {
			window.removeEventListener('click', handleWindowClick);
		}
	});

	// Tracks the most recent loadSessions() invocation so the auto-restore can
	// order its decision after the session list without duplicating the
	// stale-pointer cleanup. Never rejects (errors are handled in loadSessions).
	let loadSessionsSettled = Promise.resolve();
	const loadSessionsRefresh = createSessionRefreshScheduler(() => loadSessionsNow());

	async function loadSessionsNow() {
		const seq = ++loadSessionsSeq;
		const run = (async () => {
			const result = await invoke('get_sessions');
			// Stale response guard: a newer loadSessions call superseded this one.
			if (seq !== loadSessionsSeq) return;
			if (result && result.sessions) {
				const before = sessionReducer.getState();
				dispatchSession({
					type: 'sessions/loaded',
					sessions: result.sessions,
					autoSelect: !before.activeSessionId && !get(newSessionIntentStore),
				});
				const after = sessionReducer.getState();
				// The active session can be ended (removed from the executor) while
				// this page is open — e.g. a follow-up message targeting a
				// terminal session is dropped server-side. Drop the stale pointer
				// so the next message starts a new session instead of hitting the
				// same terminal branch again.
				if (
					after.activeSessionId &&
					!after.sessions.some((t) => t.id === after.activeSessionId) &&
					!after.error &&
					!after.termination
				) {
					dispatchSession({ type: 'session/cleared' });
				}
			}
			// Session lifecycle changes may have reaped background actions (a session
			// ending cancels its actions without terminal events): re-sync the
			// action board so the panel drops entries that no longer exist.
			// Same for scheduled actions: fired ones are gone from the pending list.
			refreshActions();
		})().catch((e) => {
			reportError(e, { context: '+page', message: '加载会话列表失败', log: false });
		});
		loadSessionsSettled = run;
		return run;
	}

	/**
	 * Refresh immediately for explicit user actions and initial hydration. The
	 * lifecycle event handlers use scheduleLoadSessions so a burst of status
	 * events produces at most one trailing get_sessions call.
	 */
	async function loadSessions() {
		const run = loadSessionsRefresh.refresh();
		loadSessionsSettled = run;
		return run;
	}

	function scheduleLoadSessions() {
		loadSessionsRefresh.schedule();
	}

	// Auto-restore the last conversation from a previous run so reopening
	// the app shows where you left off. Skipped when a resume target is
	// pending, a session is already active, or the user explicitly started a
	// fresh conversation (新对话) and no new session has been created since.
	// Messages render as soon as `get_last_conversation` returns. Non-error
	// sessions still use `reopen_session` afterwards so follow-up messages can
	// continue; errored sessions remain read-only until Continue is requested.
	/** @param {any} resumeTarget */
	async function restoreLastConversation(resumeTarget) {
		if (
			resumeTarget ||
			get(newSessionIntentStore) ||
			(browser && localStorage.getItem(NEW_ACTION_INTENT_KEY))
		) {
			return;
		}
		// Wait for the session list first so the stale-activeSessionId check below
		// sees the real list (matches the previous sequential ordering) and a
		// running/paused session auto-assigned by loadSessions wins over the restore.
		await loadSessionsSettled;
		const current = sessionReducer.getState();
		if (
			current.activeSessionId &&
			!current.sessions.some((t) => t.id === current.activeSessionId)
		) {
			dispatchSession({ type: 'session/cleared' });
		}
		if (sessionReducer.getState().activeSessionId) return;
		let last;
		try {
			last = await invoke('get_last_conversation');
		} catch (e) {
			logger.warn('+page', 'auto-restore conversation error', e);
			return;
		}
		// A session event or a later loadSessions auto-assigned one meanwhile — or
		// the user clicked the new-session button while the lookup was in flight
		// — don't clobber the live session (or the fresh draft) with the restored
		// conversation.
		if (
			!last?.session ||
			sessionReducer.getState().activeSessionId ||
			get(newSessionIntentStore)
		)
			return;
		// A completed conversation is history: the user already ended it, so
		// restoring it into the window adds nothing (and reopens it as
		// Paused, resurrecting an ended session). It stays reachable via the
		// history page; the window starts blank instead.
		if (last.session.status === 'completed') return;
		const wasError = isErrorStatus(last.session.status);
		dispatchSession({
			type: 'session/messages/resume-loaded',
			sessionId: last.session.id,
			messages: buildResumeMessages(last),
			interactions: resumeInteractions(last),
			preserveInteractionIds: pendingInteractionIdsForSession(last.session.id),
			usage: last.usage,
			llmUsage: last.llm_usage,
		});
		dispatchSession({ type: 'session/selected', sessionId: last.session.id });
		if (wasError) {
			dispatchSession({
				type: 'session/error-shown',
				sessionId: last.session.id,
				reason:
					getSessionErrorReason(last.session.id) ||
					'本次会话因错误停止，暂未收到更具体的原因。',
			});
			retainErroredSession({
				sessionId: last.session.id,
				summary: last.session.input_text,
				title: last.session.title,
			});
		}
		try {
			if (!wasError) await invoke('reopen_session', { sessionId: last.session.id });
		} catch (e) {
			logger.warn('+page', 'reopen_session error', e);
		}
		await loadSessions();
	}

	// Deliver a user message to the backend. Shared by the normal send
	// button and the queued follow-up flush (which sends a stashed message
	// once the agent's current output completes).
	/** @param {string} text @param {any} [images] @param {any} [files] */
	async function submitMessage(text, images, files) {
		try {
			const result = await submitTranscript(text, { images, files, reducer: sessionReducer });
			const createdId = processResultSessionId(result);
			if (createdId) {
				dispatchSession({ type: 'session/selected', sessionId: createdId });
				// The submission itself created the session (submitTranscript
				// already cleared the intent store): nothing to do here.
			}
			loadSessions();
		} catch (e) {
			reportError(e, { context: '+page', message: '发送失败', log: false });
		}
	}

	// True when every currently awaiting ask card has at least one selected
	// option — InputRouter then allows Enter with an empty draft.
	let askSelectionsReady = $state(false);
	const askInteraction = createAskInteractionController({
		getActiveSessionId: () => activeSessionId,
		setAutoFollow: () => {
			autoFollow = true;
		},
		setSelectionsReady: (ready) => {
			askSelectionsReady = ready;
		},
		submitMessage,
		reducer: sessionReducer,
	});
	const {
		clearAskAwaiting,
		computeAskSelectionsReady,
		handleAskSelectionChange,
		handleAskSubmit,
		handleIgnoreAsk,
		trySubmitAskSelections,
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
	/** @param {{ text: string, images: any, files: any }} payload */
	function handleInputSubmit({ text, images, files }) {
		autoFollow = true;
		if (activeSessionId && trySubmitAskSelections(activeSessionId, text, images, files)) {
			return;
		}
		submitMessage(text, images, files);
	}

	/** @param {any} session */
	function sessionStatusLabel(session) {
		if (session.status === 'running') return '运行中';
		if (isErrorStatus(session.status)) return '错误';
		if (
			session.status === 'paused' &&
			Object.values(actionsById).some(
				(action) =>
					action &&
					action.kind !== 'scheduled' &&
					action.status === 'running' &&
					action.sessionId === session.id,
			)
		)
			return '等待后台任务';
		return isPausedStatus(session.status) ? '已暂停' : '等待中';
	}

	const activeSession = $derived(
		activeSessionId ? sessions.find((session) => session.id === activeSessionId) : null,
	);
	const sessionHeaderTitle = $derived(
		String(activeSession?.title || activeSession?.input || '新会话'),
	);
	const activeConversationStatus = $derived(
		activeSession ? sessionStatusLabel(activeSession) : '就绪',
	);
	$effect(() => {
		activeConversationStatusStore.set(activeConversationStatus);
	});
</script>

<div class="chat-page" bind:this={chatPageEl}>
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
					role: '',
					content: '',
					msgId: '',
				};
		}}
	/>

	<SessionHeader
		title={sessionHeaderTitle}
		hasSession={!!activeSessionId && !activeSessionTermination}
		onNew={newSession}
		onEnd={endSession}
	/>

	<div class="messages-wrap">
		<div
			class="messages-area"
			bind:this={messagesEl}
			role="region"
			aria-label="会话消息"
			onscroll={onScroll}
			onpointerdown={cancelJumpToBottom}
			onwheel={cancelJumpToBottom}
			use:dragScroll={{ axis: 'y' }}
		>
			<ConversationTimeline
				{messages}
				mediaPlans={activeSessionId ? mediaPlansBySession[activeSessionId] || [] : []}
				loading={initialLoading}
				{hotkeyBinding}
				{awaitingBackground}
				{awaitingBackgroundCount}
				{activeSessionError}
				{sessionErrorReason}
				terminationStatus={activeSessionTermination?.status || null}
				terminationReason={activeSessionTermination?.reason || ''}
				{showContinueButton}
				{continueDisabled}
				continueBusy={continuePending}
				onContextMenu={handleContextMenu}
				onAskSelectionChange={handleAskSelectionChange}
				onIgnore={handleIgnoreAsk}
				onAskSubmit={handleAskSubmit}
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
					onclick={jumpToBottom}
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
			<SessionToolbar
				{activeSessionId}
				{showSessionMenu}
				{sessionMenuOpen}
				{menuSessions}
				onToggleSessionMenu={() => {
					if (showSessionMenu) sessionMenuOpen = !sessionMenuOpen;
					else newSession();
				}}
				onSwitchSession={switchToSession}
				{sessionStatusLabel}
				{tokenStats}
				{tokenUsageDetails}
				{tokenStatsHint}
				{buildTokenTooltip}
			/>
		{/snippet}
		{#snippet toolbarRight()}
			<ModelToolbar
				{modelMenuOpen}
				{currentModelName}
				{currentModelId}
				{modelOptions}
				onToggleMenu={() => (modelMenuOpen = !modelMenuOpen)}
				onModelSelect={handleModelSelect}
				{effortOptions}
				{currentEffort}
				onEffortSelect={handleEffortSelect}
				{webSearchSupported}
				{webSearchOptions}
				{currentWebSearch}
				onWebSearchSelect={handleWebSearchSelect}
			/>
		{/snippet}
	</Composer>
</div>

<style>
	.chat-page {
		position: relative;
		display: flex;
		flex-direction: column;
		flex: 1;
		width: 100%;
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
		user-select: none;
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
</style>
