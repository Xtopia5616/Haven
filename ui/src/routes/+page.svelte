<script>
	import logger from '$lib/logger.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { formatError } from '$lib/formatError.ts';
	import {
		buildResumeMessages,
		mergeLiveStreaming,
		isDisplayOnlyMessageId,
	} from '$lib/resumeMessages.ts';
	import {
		pickContinueStrategy,
		shouldResubmitOriginalUser,
		shouldShowContinueButton,
	} from '$lib/continueSession.ts';
	import { isBusyStatus, isPausedStatus } from '$lib/sessionStatus.ts';
	import { processResultSessionId, submitTranscript } from '$lib/submit.ts';
	import { createChatAgentEventHandlers } from '$lib/chatAgentEventHandlers.ts';
	import { createChatConfirmationEventHandlers } from '$lib/chatConfirmationEventHandlers.ts';
	import { createAskInteractionController } from '$lib/chatAskInteraction.ts';
	import { createChatSessionEventHandlers } from '$lib/chatSessionEventHandlers.ts';
	import { createChatUsageEventHandlers } from '$lib/chatUsageEventHandlers.ts';
	import { createChatModelSync } from '$lib/chatModelSync.ts';
	import { createStreamEventAggregator } from '$lib/streamAggregator.ts';
	import { buildTokenUsageTooltip } from '$lib/sessionUsagePresentation.ts';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { get } from 'svelte/store';
	import { invoke } from '$lib/tauri.ts';
	import {
		actionEventListeners,
		agentEventListeners,
		appEventListeners,
		registerListeners,
		sessionEventListeners,
	} from '$lib/events.ts';
	import {
		sessionStore,
		activeConversationStatusStore,
		addNotification,
		resumeTargetStore,
		activeSessionIdStore,
		updateModelState,
		modelStateStore,
		refreshActions,
		finalizeBackgroundActionMessages,
		actionStore,
		NEW_ACTION_INTENT_KEY,
		newSessionIntentStore,
	} from '$lib/stores.ts';
	import {
		sessionMessagesStore,
		updateSessionMessages,
		adoptDraftMessages,
		clearSessionMessages,
		clearSeqMap,
		DRAFT_KEY,
	} from '$lib/sessionMessages.ts';
	import {
		sessionTokenStatsStore,
		clearSessionTokenStats,
		restoreSessionTokenStats,
		sessionLlmUsageStore,
		restoreSessionLlmUsage,
		clearSessionLlmUsage,
		formatTokenCount,
		coalesceTokenTotal,
	} from '$lib/sessionUsage.ts';
	import { syncStore, syncStoreImmediate } from '$lib/syncStore.ts';
	import { dragScroll } from '$lib/dragScroll.ts';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import RollbackDialog from '$lib/RollbackDialog.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import SessionToolbar from '$lib/SessionToolbar.svelte';
	import ModelToolbar from '$lib/ModelToolbar.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import SessionHeader from '$lib/SessionHeader.svelte';
	import ConversationTimeline from '$lib/ConversationTimeline.svelte';
	import Composer from '$lib/Composer.svelte';

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
	let sessions = /** @type {Array<any>} */ ($state([]));
	// Pending security confirmations not yet shown, in arrival order. A
	// batched ReAct step can fire several gated tool calls at once; each one
	// must wait for its own user answer, so they are queued and displayed one
	// at a time instead of auto-rejecting the visible dialog.
	let confirmQueue =
		/** @type {Array<import('$lib/chatConfirmationEventHandlers.ts').ConfirmationQueueEntry>} */ (
			$state([])
		);
	// Interactive countdown for the visible dialog. Starts when the dialog is
	// shown (not when the request arrived) so queued confirms are not starved.
	// Backend uses a longer absolute fail-closed ceiling for closed UI.
	const CONFIRM_TIMEOUT_MS = 120_000;
	let confirmDialog =
		/** @type {{ stepId: string | null, toolName: string, sessionId: string, sessionTitle: string, riskLevel: string, params: unknown, permissionKey: string, deadlineAt: number | null }} */ (
			$state({
				stepId: null,
				toolName: '',
				sessionId: '',
				sessionTitle: '',
				riskLevel: 'medium',
				params: null,
				permissionKey: '',
				deadlineAt: null,
			})
		);
	let activeSessionId = $state(get(activeSessionIdStore));
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

	// Active session token stats (mirrored from sessionTokenStatsStore so this page
	// can render a compact budget widget). Cleared when the active session
	// changes; updated on every `agent:usage` event.
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
	 * @property {boolean} [estimated] - totals restored from a rough backend
	 *   estimate (session predates usage persistence), not real recorded usage.
	 * @property {boolean} [restored] - entry came from persistence (resume /
	 *   reopened conversation) with no live `agent:usage` events expected:
	 *   the widget shows the cumulative total instead of the per-step context.
	 */

	/** @type {SessionTokenStats | null} */
	let tokenStats = $state(null);
	$effect(() =>
		syncStore(sessionTokenStatsStore, (m) => {
			tokenStats = activeSessionId
				? /** @type {SessionTokenStats | undefined} */ (m[activeSessionId]) || null
				: null;
		}),
	);
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
	$effect(() =>
		syncStore(sessionLlmUsageStore, (m) => {
			llmUsage = activeSessionId ? m[activeSessionId] || [] : [];
		}),
	);
	$effect(() => {
		const _ = activeSessionId;
		if (!activeSessionId) llmUsage = [];
	});

	/** @param {any} stats */
	function buildTokenTooltip(stats) {
		return buildTokenUsageTooltip(stats, llmUsage);
	}

	/**
	 * Context-window utilization for the active session. Returns
	 * `{ used, window, ratio }` where `used` is the last reported context
	 * input (prompt + exclusive cache tokens) and `window` is the model's
	 * configured budget. Returns `null` when no data is available.
	 */
	const contextBudget = $derived.by(() => {
		if (!tokenStats) return null;
		const window = tokenStats.contextWindow || 0;
		const used = tokenStats.contextTokens || tokenStats.promptTokens || 0;
		if (!window) return { used, window: 0, ratio: 0 };
		const ratio = Math.min(1, used / window);
		return { used, window, ratio };
	});

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
	// The primary number must retain its meaning across pause/resume. Context
	// usage is only the latest request and changes after the next response;
	// cumulative usage is persisted and represents the whole conversation.
	const showCumulativeTokens = true;
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
		open: false,
		x: 0,
		y: 0,
		stepNumber: null,
		content: '',
		role: '',
		msgId: '',
		selectedContent: '',
	});

	/** @param {any} ev */
	function handleContextMenu(ev) {
		ctxMenu = {
			open: true,
			x: ev.x,
			y: ev.y,
			stepNumber: ev.stepNumber,
			content: ev.content,
			role: ev.role,
			msgId: ev.messageId,
			selectedContent: ev.selectedContent || '',
		};
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
			open: false,
			x: 0,
			y: 0,
			stepNumber: null,
			content: '',
			role: '',
			msgId: '',
			selectedContent: '',
		};
	}

	let ctxMenuItems = $derived([
		...(isDisplayOnlyMessageId(ctxMenu.msgId)
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

	// Resolve a live-view user message id to its persisted DB id. New submits
	// rewrite the optimistic bubble to `msg-*` via ProcessResult.message_id;
	// this path remains as a legacy fallback for bubbles that still carry a
	// temp id (reload race / older builds). Prefer content + nearest `_ts`.
	/** @param {string} sessionId @param {string} localMsgId @param {string} clickedContent */
	async function resolveUserMessageDbId(sessionId, localMsgId, clickedContent) {
		if (!localMsgId || /^(msg|step)-/.test(localMsgId)) return localMsgId;
		const localTs = Number.parseInt(String(localMsgId).split('-')[0], 10);
		try {
			const result = await invoke('get_session_for_resume', { sessionId });
			const dbMessages = buildResumeMessages(result);
			const candidates = dbMessages.filter(
				(m) => m.role === 'user' && m.content === clickedContent && /^msg-/.test(m.id),
			);
			if (candidates.length === 0) return localMsgId;
			// Nearest `_ts` to the optimistic bubble's creation time; fall back
			// to the newest match when the local timestamp is unparsable.
			let best = candidates[candidates.length - 1];
			if (Number.isFinite(localTs)) {
				let bestDiff = Number.POSITIVE_INFINITY;
				for (const m of candidates) {
					const diff = Math.abs((m._ts || 0) - localTs);
					if (diff < bestDiff) {
						bestDiff = diff;
						best = m;
					}
				}
			}
			return best.id;
		} catch (e) {
			logger.warn('+page', 'resolveUserMessageDbId failed', e);
			return localMsgId;
		}
	}

	async function confirmRollbackAction() {
		const { stepNumber, role, content, msgId } = rollbackDialog;
		rollbackLoading = true;
		try {
			if (role === 'user') {
				// User-message rollback: pause the session and put the message
				// text back in the input box so the user can edit and re-send.
				// The backend resolves targetMessageId against persisted session
				// messages and errors when the id does not match (no more
				// content-based guessing).
				const dbMsgId = await resolveUserMessageDbId(
					/** @type {string} */ (activeSessionId),
					msgId,
					content,
				);
				await invoke('rollback_session', {
					sessionId: activeSessionId,
					targetStep: stepNumber,
					pause: true,
					targetMessageId: dbMsgId,
				});
				clearSeqMap(/** @type {string} */ (activeSessionId));
				clearStepBlockIds(activeSessionId);
				// The backend is the source of truth for what the rollback
				// deleted (target message + its whole discarded timeline);
				// rebuild from the DB instead.
				await resyncSessionMessages(activeSessionId);
				inputRouterRef?.setDraft(content);
				addNotification('已回退，请编辑后重新发送', 'info', 3000);
			} else {
				await invoke('rollback_session', {
					sessionId: activeSessionId,
					targetStep: stepNumber,
					pause: false,
					targetMessageId: msgId,
				});
				clearSeqMap(/** @type {string} */ (activeSessionId));
				clearStepBlockIds(activeSessionId);
				await resyncSessionMessages(activeSessionId);
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
	/** @param {string | null} sessionId */
	async function resyncSessionMessages(sessionId) {
		if (!sessionId) return;
		try {
			const result = await invoke('get_session_for_resume', { sessionId });
			const dbMessages = buildResumeMessages(result);
			// Rollback rebuilds the timeline from the truncated DB state, so the
			// pre-rollback live messages in `existing` are STALE: their content
			// was truncated out of the DB, so mergeLiveStreaming's content-dedup
			// would keep the old reasoning/thought blocks and append them —
			// resurrecting old "Thinking…" and pushing the re-run's fresh
			// thinking to the wrong position. Keep only live messages that are
			// STILL STREAMING (the re-run's in-flight output); everything
			// finalized is replaced by the authoritative DB copy.
			updateSessionMessages(sessionId, (existing) =>
				mergeLiveStreaming(
					dbMessages,
					existing.filter((m) => m.streaming),
				),
			);
			restoreSessionTokenStats(sessionId, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(sessionId, result.llm_usage);
		} catch (e) {
			reportError(e, { context: '+page', message: '同步消息失败', log: false });
		}
	}

	function newSession() {
		if (activeSessionId) {
			clearSessionMessages(activeSessionId);
			clearSessionTokenStats(activeSessionId);
			clearSessionLlmUsage(activeSessionId);
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
		activeSessionId = null;
		activeSessionIdStore.set(null);
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
		clearSessionMessages(sessionId);
		clearSessionTokenStats(sessionId);
		clearSessionLlmUsage(sessionId);
		clearSeqMap(sessionId);
		clearStepBlockIds(sessionId);
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
			const dbMessages = buildResumeMessages(result);
			// Live tool cards and DB step badges share the same `step-*` id
			// (minted by the backend when the action started), so the merge
			// dedups them by id alone — a mid-step card keeps streaming its
			// observation, the DB copy wins once it is finalized.
			updateSessionMessages(sessionId, (existing) =>
				mergeLiveStreaming(dbMessages, existing),
			);
			restoreSessionTokenStats(sessionId, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(sessionId, result.llm_usage);
			// An explicit switch abandons the fresh-start intent: the chosen
			// session becomes the active conversation (and may be auto-restored
			// on the next app launch).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			activeSessionId = sessionId;
			activeSessionIdStore.set(sessionId);
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
					prevSession.status === 'error' ||
					prevSession.status === 'failed'
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
			clearSessionMessages(endedId);
			clearSessionTokenStats(endedId);
			clearSessionLlmUsage(endedId);
			clearSeqMap(endedId);
			clearStepBlockIds(endedId);
		} catch (e) {
			// The session is still alive server-side: keep the view attached to
			// it so the user can retry. Clearing the pointer here would orphan a
			// session that keeps running (and streaming) with no visible target.
			newSessionIntentStore.set(false);
			reportError(e, { context: '+page', message: '结束会话失败', log: false });
			return;
		}
		activeSessionId = null;
		activeSessionIdStore.set(null);
		newSessionIntentStore.set(false);
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
		const currentMessages = get(sessionMessagesStore)[tid] || [];
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
			sessionErrorId = null;
			activeSessionError = false;
			// Re-sync from the authoritative post-continue DB state. Any retry
			// stream that won the race with this request has a fresh id and is
			// retained; stale pre-continue UI entries cannot leak back in.
			try {
				const result = await invoke('get_session_for_resume', { sessionId: tid });
				updateSessionMessages(tid, (existing) => {
					const dbMessages = buildResumeMessages(result);
					const retryMessages = existing.filter((m) => !preContinueMessageIds.has(m.id));
					return mergeLiveStreaming(dbMessages, retryMessages);
				});
			} catch (e) {
				// Keep the current view until a later sync succeeds. A failed read
				// is not evidence that any visible history is a failed partial.
			}
			clearSeqMap(tid);
			clearStepBlockIds(tid);
			// Two strategies:
			// - LLM mid-generation interrupt → send "继续" as a real user turn.
			// - User message sent but agent never generated → pass the original
			//   text (resubmit only when it did not survive as a persisted
			//   trailing user turn; otherwise Pending resume alone retries).
			autoFollow = true;
			if (strategy.mode === 'continue') {
				submitMessage(strategy.text, []);
			} else {
				const synced = get(sessionMessagesStore)[tid] || [];
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
	let dead = false;
	// Guards concurrent loadSessions() calls so a stale response can't overwrite
	// a newer one.
	let loadSessionsSeq = 0;

	// Sync the Svelte store to a $state variable — $effect does NOT track
	// get(store), so we must use .subscribe() to get reactive updates.
	// Also read the current value once on mount via get(), otherwise values
	// set before subscription (e.g. by history resume) are never received.
	/** @type {Record<string, any[]>} */
	let sessionMessagesDict = $state({});
	$effect(() =>
		syncStoreImmediate(
			sessionMessagesStore,
			(v) => {
				sessionMessagesDict = v;
			},
			() => get(sessionMessagesStore),
		),
	);

	// Derive visible messages for the current view.
	$effect(() => {
		const dict = sessionMessagesDict;
		if (activeSessionId) {
			messages = Array.isArray(dict[activeSessionId]) ? dict[activeSessionId] : [];
		} else {
			messages = Array.isArray(dict[DRAFT_KEY]) ? dict[DRAFT_KEY] : [];
		}
	});

	let activeSessionError = $state(false);
	let sessionErrorId = /** @type {string | null} */ ($state(null));
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
			sessionErrorId = null;
			activeSessionError = false;
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

	// Persist activeSessionId across page navigations via store.
	$effect(() => {
		activeSessionIdStore.set(activeSessionId);
	});

	// Follow external store writes back into the local state. The effect
	// above mirrors state → store only; submit.ts writes the store directly
	// when a submission creates a fresh session (its `SessionCreated` result never
	// passes through this page), and the view must follow the new session
	// instead of staying on the blank draft. Guarded with `!activeSessionId`
	// (never override a session the user is actively viewing) AND the
	// fresh-start intent (while the intent is pending, a background session
	// creation must not hijack the blank draft — the submission that
	// fulfills the intent clears it before writing the store).
	$effect(() =>
		syncStore(activeSessionIdStore, (id) => {
			if (id) {
				if (!activeSessionId && !get(newSessionIntentStore)) activeSessionId = id;
			} else if (activeSessionId) {
				// An external writer nulled the store — the only path is the
				// history page deleting/clearing the session that was active.
				// Follow it so the chat never keeps pointing at a session that
				// no longer exists (the +page's own writers always set the
				// local state first, so this can never clobber a live view).
				activeSessionId = null;
			}
		}),
	);

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
		const threshold = 100;
		const atBottom =
			messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < threshold;
		autoFollow = atBottom;
	}

	function jumpToBottom() {
		autoFollow = true;
		if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
	}

	const streamEvents = createStreamEventAggregator({
		getActiveSessionId: () => activeSessionId,
		onActiveStream: () => updateModelState('streaming'),
	});
	const { blockIdsOf, chunkHandler, clearStepBlockIds, flushChunksNow } = streamEvents;

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
	function processResumeTarget(resumeTarget) {
		if (resumeTarget && resumeTarget.sessionId) {
			// Opening a reviewed conversation abandons any pending fresh-start
			// intent (the user chose this conversation explicitly).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			const prevActive = activeSessionId;
			activeSessionId = resumeTarget.sessionId;
			activeSessionIdStore.set(activeSessionId);
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
					prevSession.status === 'error' ||
					prevSession.status === 'failed'
				) {
					evictTerminalSessionMemory(prevActive);
				}
			}
			// If this session was errored when reviewed, show the continue button.
			// reopen_session already set it to Paused, but we still want the user
			// to see the option to retry the failed step.
			if (resumeTarget.wasError) {
				sessionErrorId = resumeTarget.sessionId;
				activeSessionError = true;
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
						adoptDraftMessages,
						setActiveSessionId: (sessionId) => {
							activeSessionId = sessionId;
							activeSessionIdStore.set(sessionId);
						},
						getSessionErrorId: () => sessionErrorId,
						clearSessionError: () => {
							sessionErrorId = null;
							activeSessionError = false;
						},
						showSessionError: (sessionId) => {
							sessionErrorId = sessionId;
							activeSessionError = true;
						},
						clearAskAwaiting,
						evictTerminalSessionMemory,
						clearStepBlockIds,
						flushChunksNow,
						updateSessionTitle: (sessionId, title) => {
							const index = sessions.findIndex((session) => session.id === sessionId);
							if (index >= 0) {
								sessions[index] = { ...sessions[index], title };
								// Keep the shell's task/status view in sync with the chat
								// header as soon as the generated title arrives.
								sessionStore.set(sessions);
							} else {
								// A title event can win the race with the initial session
								// list load. The persisted title will be picked up here.
								void loadSessions();
							}
						},
						loadSessions,
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
						getActiveSessionId: () => activeSessionId,
						blockIdsOf,
						chunkHandler,
						flushChunksNow,
					}),
				),
				...actionEventListeners({
					'action:finished': (event) => {
						// Persist terminal background output onto the tool card and
						// clear actionId so a later refreshActions() cannot revert
						// the card to the original "running" observation ack.
						finalizeBackgroundActionMessages(event.payload);
					},
				}),
				...appEventListeners(
					createChatConfirmationEventHandlers({
						getSessionTitle: (sessionId) =>
							sessions.find((session) => session.id === sessionId)?.title ||
							sessionId,
						enqueueConfirmation: (entry) => {
							confirmQueue = [...confirmQueue, entry];
						},
						showNextConfirm,
					}),
				),
				...agentEventListeners(createChatUsageEventHandlers()),
			},
			{ tag: '+page' },
		);
		eventRegistrations = registrations;
		const readyP = registrations.ready;

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
			await Promise.all([sessionsP, restoreP, readyP]);
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
		// Flush any queued streaming chunks so the in-memory message store is
		// complete before the listeners are disposed (a re-entry to this page
		// merges the store with the DB copy).
		flushChunksNow();
		eventRegistrations?.dispose();
		if (browser) {
			window.removeEventListener('click', handleWindowClick);
		}
	});

	// Tracks the most recent loadSessions() invocation so the auto-restore can
	// order its decision after the session list without duplicating the
	// stale-pointer cleanup. Never rejects (errors are handled in loadSessions).
	let loadSessionsSettled = Promise.resolve();

	async function loadSessions() {
		const seq = ++loadSessionsSeq;
		const run = (async () => {
			const result = await invoke('get_sessions');
			// Stale response guard: a newer loadSessions call superseded this one.
			if (seq !== loadSessionsSeq) return;
			if (result && result.sessions) {
				sessions = result.sessions;
				sessionStore.set(sessions);
				// The active session can be ended (removed from the executor) while
				// this page is open — e.g. a follow-up message targeting a
				// terminal session is dropped server-side. Drop the stale pointer
				// so the next message starts a new session instead of hitting the
				// same terminal branch again.
				if (activeSessionId && !sessions.some((t) => t.id === activeSessionId)) {
					activeSessionId = null;
					activeSessionIdStore.set(null);
				}
				if (!activeSessionId && !get(newSessionIntentStore)) {
					// Only auto-assign a session whose messages are actually in
					// memory. A session that has no loaded messages here (e.g.
					// its list was cleared by an earlier 新对话) must NOT be
					// silently activated: the chat would render the blank
					// welcome screen while activeSessionId still points at it,
					// so the next typed message would be appended to that
					// hidden session and the end button would target it.
					const firstActive = sessions.find(
						(t) =>
							(isBusyStatus(t.status) || isPausedStatus(t.status)) &&
							(get(sessionMessagesStore)[t.id] || []).length > 0,
					);
					if (firstActive) {
						activeSessionId = firstActive.id;
					}
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

	// Auto-restore the last conversation from a previous run so reopening
	// the app shows where you left off. Skipped when a resume target is
	// pending, a session is already active, or the user explicitly started a
	// fresh conversation (新对话) and no new session has been created since.
	// Messages render as soon as `get_last_conversation` returns; the
	// follow-up `reopen_session` (which only lets follow-up messages continue
	// this session instead of being dropped as a terminal-session supplement) runs
	// afterwards without blocking the UI.
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
		if (activeSessionId && !sessions.some((t) => t.id === activeSessionId)) {
			activeSessionId = null;
			activeSessionIdStore.set(null);
		}
		if (activeSessionId) return;
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
		if (!last?.session || activeSessionId || get(newSessionIntentStore)) return;
		// A completed conversation is history: the user already ended it, so
		// restoring it into the window adds nothing (and reopens it as
		// Paused, resurrecting an ended session). It stays reachable via the
		// history page; the window starts blank instead.
		if (last.session.status === 'completed') return;
		const wasError = last.session.status === 'error' || last.session.status === 'failed';
		updateSessionMessages(last.session.id, (existing) =>
			mergeLiveStreaming(buildResumeMessages(last), existing),
		);
		restoreSessionTokenStats(last.session.id, last.usage, last.usage_estimated);
		restoreSessionLlmUsage(last.session.id, last.llm_usage);
		activeSessionId = last.session.id;
		activeSessionIdStore.set(activeSessionId);
		if (wasError) {
			sessionErrorId = last.session.id;
			activeSessionError = true;
		}
		try {
			await invoke('reopen_session', { sessionId: last.session.id });
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
			const result = await submitTranscript(text, { images, files });
			const createdId = processResultSessionId(result);
			if (createdId) {
				activeSessionId = createdId;
				activeSessionIdStore.set(activeSessionId);
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

	// Show the next queued confirmation once the current one is resolved
	// (either by the user or by the dialog's timeout). Entries are shown in
	// arrival order so every pending operation still gets its own decision.
	function showNextConfirm() {
		if (confirmDialog.stepId || confirmQueue.length === 0) return;
		const [next, ...rest] = confirmQueue;
		confirmQueue = rest;
		confirmDialog = {
			...next,
			// Fresh 120s window from show time — queued items keep a full
			// interactive budget instead of inheriting arrival-time debt.
			deadlineAt: Date.now() + CONFIRM_TIMEOUT_MS,
		};
	}

	/** @param {{ stepId: string, approved: boolean, effect?: string, scope?: string, trustSession?: boolean }} payload */
	async function handleConfirm({ stepId, approved, effect, scope, trustSession }) {
		// Clear the dialog synchronously BEFORE awaiting the IPC round-trip.
		// If we only cleared it after `await invoke(...)`, a new
		// `confirm:requested` arriving during that window would find the old
		// stepId still set and hold the queue hostage until the stale dialog
		// was dismissed.
		const resolvedStep = stepId;
		confirmDialog = {
			stepId: null,
			toolName: '',
			sessionId: '',
			sessionTitle: '',
			riskLevel: 'medium',
			params: null,
			permissionKey: '',
			deadlineAt: null,
		};
		// Surface the next queued confirmation immediately (before the IPC
		// await) so a batched step's remaining operations stay answerable
		// back-to-back instead of piling up behind the in-flight resolve.
		showNextConfirm();
		if (!resolvedStep) return;
		const resolvedEffect = effect || (approved ? 'allow' : 'deny');
		const resolvedScope = scope || (trustSession ? 'session' : 'once');
		try {
			await invoke('resolve_confirmation', {
				stepId: resolvedStep,
				confirmed: approved,
				trustSession: resolvedScope === 'session' && resolvedEffect === 'allow',
				effect: resolvedEffect,
				scope: resolvedScope,
			});
		} catch (e) {
			reportError(e, { context: '+page', message: '确认失败', log: false });
		}
	}

	/** @param {any} session */
	function sessionStatusLabel(session) {
		if (session.status === 'running') return '运行中';
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
	const sessionHeaderTitle = $derived(activeSession?.title || activeSession?.input || '新会话');
	const activeConversationStatus = $derived(
		activeSession ? sessionStatusLabel(activeSession) : '就绪',
	);
	$effect(() => {
		activeConversationStatusStore.set(activeConversationStatus);
	});
</script>

<div class="chat-page">
	<ConfirmationDialog
		stepId={confirmDialog.stepId}
		toolName={confirmDialog.toolName}
		sessionId={confirmDialog.sessionId}
		sessionTitle={confirmDialog.sessionTitle}
		riskLevel={confirmDialog.riskLevel}
		params={confirmDialog.params}
		permissionKey={confirmDialog.permissionKey}
		deadlineAt={confirmDialog.deadlineAt}
		onConfirm={handleConfirm}
	/>

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

	<ContextMenu
		open={ctxMenu.open}
		x={ctxMenu.x}
		y={ctxMenu.y}
		items={ctxMenuItems}
		onClose={closeCtxMenu}
	/>

	<SessionHeader
		title={sessionHeaderTitle}
		hasSession={!!activeSessionId}
		onNew={newSession}
		onEnd={endSession}
	/>

	<div class="messages-wrap">
		<div
			class="messages-area"
			bind:this={messagesEl}
			onscroll={onScroll}
			use:dragScroll={{ axis: 'y' }}
		>
			<ConversationTimeline
				{messages}
				loading={initialLoading}
				{hotkeyBinding}
				{awaitingBackground}
				{awaitingBackgroundCount}
				{activeSessionError}
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
			<MaterialIconButton
				size="toolbar"
				variant="tonal"
				className="jump-bottom"
				label="返回底部"
				title="返回底部"
				onclick={jumpToBottom}
			>
				<svg
					width="18"
					height="18"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					><path d="M12 5v14" /><polyline points="19 12 12 19 5 12" /></svg
				>
			</MaterialIconButton>
		{/if}
	</div>

	<Composer
		bind:this={inputRouterRef}
		{activeSessionId}
		{hotkeyBinding}
		{isGenerating}
		{sessionRunning}
		interrupting={interruptPending}
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
				onNewSession={newSession}
				onSwitchSession={switchToSession}
				{sessionStatusLabel}
				{tokenStats}
				{tokenStatsHint}
				{buildTokenTooltip}
				{formatTokenCount}
				{coalesceTokenTotal}
				{showCumulativeTokens}
				{contextBudget}
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
		padding: var(--md-sys-space-lg) var(--md-sys-space-md);
	}
	:global(.messages-area.drag-scroll--active) {
		cursor: grabbing;
		user-select: none;
	}
	:global(.jump-bottom) {
		position: absolute;
		right: var(--md-sys-space-md);
		bottom: var(--md-sys-space-sm);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-2);
		transition: background var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
		z-index: 5;
	}
</style>
