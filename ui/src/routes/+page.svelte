<script module>
	// Per-session cache for the toolbar model switcher's model discovery.
	// Dev-mode page reloads (Vite HMR reconnect, window re-show, single-
	// instance re-entry) remount the chat view and would otherwise fire a
	// duplicate discover_models request against the same default endpoint.
	// Cache the result per base URL and share in-flight requests so reloads
	// reuse the list instead of re-hitting the provider's /models endpoint.
	/** @type {{ baseUrl: string | null, list: any[] | null, inflight: Promise<any> | null, inflightUrl: string | null }} */
	const defaultModelsCache = {
		baseUrl: null,
		list: null,
		inflight: null,
		inflightUrl: null,
	};
</script>

<script>
	import logger from '$lib/logger.ts';
	import { formatError } from '$lib/formatError.ts';
	import { buildResumeMessages, mergeLiveStreaming } from '$lib/resumeMessages.ts';
	import { pickContinueStrategy, shouldResubmitOriginalUser } from '$lib/continueSession.ts';
	import { isBusyStatus, isPausedStatus } from '$lib/sessionStatus.ts';
	import { processResultSessionId, submitTranscript } from '$lib/submit.ts';
	import {
		applyThoughtSnap,
		webSearchId,
		webSearchCardContent,
		finalizeStreamBlocks,
		dropStreamedThought,
		insertAgentMessage,
		newToolMessage,
		actionIdFromObservation,
		parseActionResultInject,
	} from '$lib/streaming.ts';
	import { createStreamEventAggregator } from '$lib/streamAggregator.ts';
	import {
		buildTokenUsageTooltip,
		stepUsageFor,
	} from '$lib/sessionUsagePresentation.ts';
	import { normalizeApiStyle, supportsBuiltinWebSearch } from '$lib/apiStyle.ts';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { fly } from 'svelte/transition';
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
		sessionMessagesStore,
		sessionStore,
		addNotification,
		updateSessionMessages,
		adoptDraftMessages,
		clearSessionMessages,
		clearSeqMap,
		resumeTargetStore,
		activeSessionIdStore,
		sessionTokenStatsStore,
		updateSessionTokenStats,
		clearSessionTokenStats,
		restoreSessionTokenStats,
		sessionLlmUsageStore,
		restoreSessionLlmUsage,
		clearSessionLlmUsage,
		formatTokenCount,
		coalesceTokenTotal,
		pruneSeq,
		updateModelState,
		modelStateStore,
		refreshActions,
		setToolOutputPreview,
		clearToolOutputPreview,
		appendSessionLlmUsage,
		finalizeBackgroundActionMessages,
		actionStore,
		DRAFT_KEY,
		NEW_ACTION_INTENT_KEY,
		newSessionIntentStore,
	} from '$lib/stores.ts';
	import { syncStore, syncStoreImmediate } from '$lib/syncStore.ts';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import RollbackDialog from '$lib/RollbackDialog.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import Logo from '$lib/Logo.svelte';
	import InputRouter from '$lib/InputRouter.svelte';
	import SessionToolbar from '$lib/SessionToolbar.svelte';
	import ModelToolbar from '$lib/ModelToolbar.svelte';

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
	let sessions = /** @type {Array<any>} */ ($state([]));
	// Pending security confirmations not yet shown, in arrival order. A
	// batched ReAct step can fire several gated tool calls at once; each one
	// must wait for its own user answer, so they are queued and displayed one
	// at a time instead of auto-rejecting the visible dialog.
	let confirmQueue = /** @type {Array<any>} */ ($state([]));
	// Interactive countdown for the visible dialog. Starts when the dialog is
	// shown (not when the request arrived) so queued confirms are not starved.
	// Backend uses a longer absolute fail-closed ceiling for closed UI.
	const CONFIRM_TIMEOUT_MS = 120_000;
	let confirmDialog = $state({
		stepId: null,
		toolName: '',
		sessionId: '',
		sessionTitle: '',
		riskLevel: 'medium',
		params: null,
		permissionKey: '',
		deadlineAt: null,
	});
	let activeSessionId = $state(get(activeSessionIdStore));
	let rollbackDialog = $state({ open: false, stepNumber: null, role: '', content: '', msgId: '' });
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
	// persisted `llm_usage` when a resume conversation opens). Used to render
	// per-step token chips on tool cards and the tooltip call count.
	/** @type {Array<import('$lib/stores.ts').LlmUsage>} */
	let llmUsage = $state([]);
	$effect(() =>
		syncStore(sessionLlmUsageStore, (m) => {
			llmUsage = activeSessionId ? (m[activeSessionId] || []) : [];
			stepUsageCache.clear();
		}),
	);
	$effect(() => {
		const _ = activeSessionId;
		if (!activeSessionId) llmUsage = [];
	});

	const stepUsageCache = new Map();
	/** @param {number|null} stepNumber */
	function stepUsage(stepNumber) {
		return stepUsageFor(llmUsage, stepNumber, stepUsageCache);
	}
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

	// Send/stop merged button: text takes priority (always send); with no
	// text and the agent actively generating output the button becomes
	// "stop session". Also mirrors the agent's model state so the button can
	// distinguish "generating right now" from an idle running session.
	let modelState = $state('ready');
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
	// Sessions executing in parallel (running or waiting). When 2+ exist, the
	// new-session button turns into a switcher menu: switch to a parallel session
	// or start a new one. Otherwise the button keeps its default behavior.
	const parallelSessions = $derived(sessions.filter((t) => isBusyStatus(t.status)));
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
		activeSessionId
			? sessions.find((t) => t.id === activeSessionId)?.status
			: undefined,
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
			addNotification(`设置联网搜索失败: ${formatError(e)}`, 'error', 4000);
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
			addNotification(`切换模型失败: ${formatError(e)}`, 'error', 4000);
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
			addNotification(`设置思考强度失败: ${formatError(e)}`, 'error', 4000);
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
			} catch {
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
		{ id: 'rollback', label: '回退到此消息', icon: 'rollback', action: handleCtxRollback },
		{ id: 'copy', label: '复制', icon: 'copy', action: handleCtxCopy },
	]);

	/** @param {MouseEvent} e */
	function handleWindowClick(e) {
		if (modelMenuOpen) {
			const menu = document.querySelector('.model-menu');
			const btn = document.querySelector('.model-switch-btn');
			if (menu && btn && !menu.contains(/** @type {Node} */ (e.target)) && !btn.contains(/** @type {Node} */ (e.target))) {
				modelMenuOpen = false;
			}
		}
		if (sessionMenuOpen) {
			const menu = document.querySelector('.session-menu');
			const btn = document.querySelector('.session-switch-btn');
			if (menu && btn && !menu.contains(/** @type {Node} */ (e.target)) && !btn.contains(/** @type {Node} */ (e.target))) {
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
			addNotification(`回退失败: ${formatError(e)}`, 'error', 5000);
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
				mergeLiveStreaming(dbMessages, existing.filter((m) => m.streaming)),
			);
			restoreSessionTokenStats(sessionId, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(sessionId, result.llm_usage);
		} catch (e) {
			addNotification(`同步消息失败: ${formatError(e)}`, 'error', 3000);
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
			addNotification(`切换会话失败: ${formatError(e)}`, 'error', 4000);
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
			addNotification(`结束会话失败: ${formatError(e)}`, 'error', 3000);
			return;
		}
		activeSessionId = null;
		activeSessionIdStore.set(null);
		newSessionIntentStore.set(false);
	}

	async function handleContinue() {
		if (!activeSessionId) return;
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
			addNotification(`继续失败: ${formatError(e)}`, 'error', 5000);
			// Keep the banner visible so the user can retry.
		}
	}

	// Tauri event listener handle (registered in onMount, disposed in
	// onDestroy). See eventRegistrations below.
	let eventRegistrations = /** @type {{ ready: Promise<void>; dispose: () => void } | null} */ (null);
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

	// Populate the toolbar model switcher from a per-session cache so page
	// reloads (dev HMR reconnect, window re-show, single-instance re-entry)
	// don't re-request the same model list. Concurrent mounts share the
	// in-flight request, so the duplicate discover_models calls seen on
	// reload disappear without losing the fresh-on-first-load behavior.
	/** @param {string} baseUrl @param {string} providerName */
	function ensureDefaultModelOptions(baseUrl, providerName) {
		if (defaultModelsCache.baseUrl === baseUrl && defaultModelsCache.list) {
			modelOptions = defaultModelsCache.list;
			return;
		}
		// Settings can swap the default provider while this view stays mounted
		// (keep-alive). Drop the previous endpoint's list; an in-flight fetch
		// for a different URL is abandoned (its .then is stamped and no-ops).
		if (defaultModelsCache.baseUrl !== baseUrl) {
			defaultModelsCache.list = null;
			defaultModelsCache.baseUrl = baseUrl;
		}
		if (defaultModelsCache.inflight && defaultModelsCache.inflightUrl === baseUrl) {
			defaultModelsCache.inflight
				.then((list) => {
					if (!dead && defaultModelsCache.baseUrl === baseUrl) modelOptions = list;
				})
				.catch(() => {
					if (!dead && defaultModelsCache.baseUrl === baseUrl) modelOptions = [];
				});
			return;
		}
		const requestedUrl = baseUrl;
		defaultModelsCache.baseUrl = requestedUrl;
		defaultModelsCache.inflightUrl = requestedUrl;
		defaultModelsCache.inflight = invoke('discover_models', {
			baseUrl: requestedUrl,
			apiKey: '',
			provider: providerName || '',
		})
			.then((list) => {
				const next = list || [];
				// Stale response after a provider swap: ignore.
				if (defaultModelsCache.baseUrl !== requestedUrl) return next;
				defaultModelsCache.list = next;
				if (!dead) modelOptions = next;
				return next;
			})
			.catch((e) => {
				logger.warn('+page', 'discover_models error', e);
				if (!dead && defaultModelsCache.baseUrl === requestedUrl) modelOptions = [];
				throw e;
			})
			.finally(() => {
				// Only clear the coalescing slot when we still own it.
				if (defaultModelsCache.inflightUrl === requestedUrl) {
					defaultModelsCache.inflight = null;
					defaultModelsCache.inflightUrl = null;
				}
			});
		// Swallow the rethrown rejection for the shared in-flight promise;
		// the branch above already surfaces the failure to the UI.
		defaultModelsCache.inflight.catch(() => {});
	}

	/**
	 * Apply the default_model role from a get_settings payload onto the
	 * toolbar switcher. Used on mount and whenever `llm:config_changed`
	 * fires (settings save / toolbar switch) so keep-alive doesn't leave
	 * the chat toolbar stuck on a stale model.
	 * @param {any} s
	 */
	function applyDefaultModelFromSettings(s) {
		const dmRole = (/** @type {any[]} */ (s?.llm?.roles || [])).find((r) => r.role === 'default_model');
		const dmProvider = dmRole?.provider
			? (/** @type {any[]} */ (s?.llm?.providers || [])).find((p) => p.name === dmRole.provider)
			: null;
		const dmModel = dmRole?.model || '';
		currentModelId = dmModel;
		currentModelName = dmModel;
		currentEffort = dmRole?.reasoning_effort || '';
		currentWebSearch = dmRole?.web_search || 'off';
		currentApiStyle = normalizeApiStyle(dmProvider?.api_style || dmProvider?.provider);
		webSearchSupported = supportsBuiltinWebSearch(currentApiStyle);
		// Stale auto/always on an unsupported style: clear to off so it cannot
		// resurrect when the user later switches to a supporting provider.
		if (!webSearchSupported && currentWebSearch !== 'off') {
			currentWebSearch = 'off';
			invoke('set_web_search', { role: 'default_model', mode: 'off' }).catch((e) => {
				logger.warn('+page', 'clear unsupported web_search failed', e);
			});
		} else if (webSearchSupported && currentApiStyle === 'gemini' && currentWebSearch === 'always') {
			// Gemini Always ≡ Auto; normalize stored value.
			currentWebSearch = 'auto';
			invoke('set_web_search', { role: 'default_model', mode: 'auto' }).catch((e) => {
				logger.warn('+page', 'normalize gemini web_search always→auto failed', e);
			});
		}
		if (dmProvider?.base_url) {
			ensureDefaultModelOptions(dmProvider.base_url, dmProvider.name);
		} else {
			modelOptions = [];
		}
	}

	// Monotonic generation so overlapping get_settings refreshes never apply
	// an older snapshot after a newer toolbar switch / settings save.
	let defaultModelSyncGen = 0;
	// Toolbar already wrote local state before emitting llm:config_changed —
	// skip the redundant self-echo refresh once.
	let skipNextDefaultModelRefresh = false;

	/** Re-fetch settings and refresh the toolbar default-model controls. */
	function refreshDefaultModelFromBackend() {
		const gen = ++defaultModelSyncGen;
		invoke('get_settings')
			.then((s) => {
				if (dead || gen !== defaultModelSyncGen) return;
				applyDefaultModelFromSettings(s);
			})
			.catch((e) => {
				logger.warn('+page', 'refresh default model error', e);
			});
	}

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
				...sessionEventListeners({
				'session:created': (event) => {
					const tid = event.payload.sessionId;
					if (tid) {
						// Voice input appends the transcript to `_draft` before the
						// backend session exists; once it is created, migrate those
						// draft messages into the session and focus it. Without this,
						// the agent's response (ask card / answer) lands in a session
						// stream the chat view is not showing — visible only after
						// re-entering the page (e.g. via history).
						adoptDraftMessages(tid);
						// Every `session:created` comes from a user submission
						// (typed or voice) — the fresh-start intent is fulfilled
						// by submit.ts when that submission's invoke resolves.
						// This guard only covers the in-flight window between the
						// session creation event and the invoke resolution: a
						// submission that started before the 新对话 click must
						// not hijack the blank draft in that window.
						if (!get(newSessionIntentStore)) {
							activeSessionId = tid;
							activeSessionIdStore.set(tid);
						}
					}
					loadSessions();
				},
				'session:updated': (event) => {
					const data = event.payload;
					const isActive = activeSessionId && data.sessionId === activeSessionId;
					// A resume (pending) means the user's answer was received:
					// stop showing the awaiting indicator on ask cards. Note the
					// ask pause itself arrives as 'paused' right after the card is
					// created, so that status must NOT clear the indicator.
					if (isActive && data.status === 'pending') {
						clearAskAwaiting(data.sessionId);
					}
					// A resumed session (pending/running) is no longer in the
					// errored state the continue banner describes: dismiss a
					// stale banner so it can't linger over a live generation
					// (e.g. when the retry started before the continue-session
					// invoke resolved, or a message resumed the session).
					if (
						sessionErrorId === data.sessionId &&
						isBusyStatus(data.status)
					) {
						sessionErrorId = null;
						activeSessionError = false;
					}
					// A background session reaching a terminal state has no more
					// streaming events: evict its messages (switchToSession reloads
					// from the DB on demand) so completed conversations don't
					// accumulate in memory for the whole session.
					if (data.status === 'completed' || data.status === 'error') {
						evictTerminalSessionMemory(data.sessionId);
						// The ACTIVE session is skipped by the eviction guard, but
						// its streaming bookkeeping is dead too: no further chunk
						// events will reference these (step, run) keys. Also drop
						// leftover carets on Thinking / thought bubbles that never
						// got an `agent:thought` snap (DeepSeek reasoning-only turns).
						if (activeSessionId === data.sessionId) {
							updateSessionMessages(data.sessionId, (m) =>
								m.map((x) => (x.streaming ? { ...x, streaming: false } : x)),
							);
						}
						clearStepBlockIds(data.sessionId);
					}
					loadSessions();
				},
				'session:completed': (event) => {
					const data = event.payload;
					if (activeSessionId && data.sessionId === activeSessionId) {
						clearAskAwaiting(data.sessionId);
						updateSessionMessages(data.sessionId, (m) =>
							m.map((x) => (x.streaming ? { ...x, streaming: false } : x)),
						);
					}
					evictTerminalSessionMemory(data.sessionId);
					clearStepBlockIds(data.sessionId);
					loadSessions();
				},
				'session:error': (event) => {
					const { sessionId } = event.payload;
					if (sessionId === activeSessionId) {
						sessionErrorId = sessionId;
						activeSessionError = true;
						clearAskAwaiting(sessionId);
						// The session died mid-tool-call: every streaming block
						// (tool placeholder, reasoning, thought) would stay
						// in its "expanded/streaming" state forever otherwise.
						// Finalize them all so the UI reflects the stop.
						updateSessionMessages(sessionId, (m) =>
							m.map((x) => (x.streaming ? { ...x, streaming: false } : x))
						);
					}
					evictTerminalSessionMemory(sessionId);
					clearStepBlockIds(sessionId);
					loadSessions();
				},
				'session:title-updated': (event) => {
					const { sessionId, title } = event.payload;
					const idx = sessions.findIndex((t) => t.id === sessionId);
					if (idx >= 0) sessions[idx] = { ...sessions[idx], title };
				},
				}),
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
				...agentEventListeners({
				'agent:thought': (event) => {
					const data = event.payload;
					const tid = data.sessionId;
					// The snap carries the minted message id the chunks streamed
					// into (and the DB row is persisted under), so the reconcile
					// is a plain id-keyed replace. The sibling reasoning id comes
					// from the block registry (a `msg-*` id carries no step info).
					const thoughtId = data.messageId;
					const { reasoningId } = blockIdsOf(tid, data.stepNumber, data.runId);
					// The authoritative snap reconciles the streamed text: apply
					// any queued chunks first so no delta is left to accumulate
					// onto the finalized message afterwards.
					flushChunksNow();
					if (thoughtId) pruneSeq(thoughtId);
					if (reasoningId) pruneSeq(reasoningId);
					// Deliberately no updateModelState here: the chunk handler
					// already left the chip in `streaming`, and forcing `ready`
					// on the thought snapshot causes a visible ready->tool flicker
					// when the step continues with tool calls. The next event
					// (agent:action -> tool, or pause/completion -> ready) owns
					// the transition.
					updateSessionMessages(tid, (m) =>
						applyThoughtSnap(m, {
							messageId: thoughtId,
							reasoningId,
							thought: data.thought,
							stepNumber: data.stepNumber,
							runId: data.runId,
							time: new Date().toLocaleTimeString(),
						}),
					);
				},
				'agent:thought_chunk': chunkHandler(true, undefined),
				'agent:reasoning_chunk': chunkHandler(false, 'reasoning'),
				'agent:web_search': (event) => {
					const data = event.payload;
					const tid = data.sessionId;
					if (!tid || (activeSessionId && tid !== activeSessionId)) return;
					// Mirror agent:action: flush queued text first, then finalize
					// the current thought/reasoning bubble so post-search deltas
					// open a NEW bubble below this card instead of appending above.
					flushChunksNow();
					const callId = data.callId || null;
					// Skip unkeyed updates when the adapter had no call id —
					// a null-id card would later collide with the real ws_* id.
					if (!callId) return;
					const wsId = webSearchId(tid, data.stepNumber, data.runId, callId);
					const placeholderId = webSearchId(tid, data.stepNumber, data.runId, null);
					const { reasoningId, thoughtId } = blockIdsOf(
						tid,
						data.stepNumber,
						data.runId,
					);
					updateSessionMessages(tid, (m) => {
						let next = m;
						let existing = next.find((x) => x.id === wsId);
						// Upgrade a legacy null-id placeholder in place when the
						// real call_id arrives (belt-and-suspenders for older
						// events that still lacked call_id).
						if (!existing) {
							const phIdx = next.findIndex(
								(x) => x.id === placeholderId && x.toolName === 'web_search',
							);
							if (phIdx >= 0) {
								next = next.map((x, i) =>
									i === phIdx ? { ...x, id: wsId } : x,
								);
								existing = next[phIdx];
							}
						}
						const content = webSearchCardContent(data, existing?.content);
						// Finalize only when opening a NEW card — later phase
						// updates for the same call_id must not re-finalize a
						// post-search bubble that already started streaming.
						if (!existing) {
							if (reasoningId) pruneSeq(reasoningId);
							if (thoughtId) pruneSeq(thoughtId);
							next = finalizeStreamBlocks(next, reasoningId, thoughtId);
						}
						if (data.phase === 'completed') {
							if (!existing) {
								return insertAgentMessage(
									next,
									newToolMessage({
										id: wsId,
									stepNumber: data.stepNumber,
										toolName: 'web_search',
										time: new Date().toLocaleTimeString(),
										content,
										streaming: false,
									}),
								);
							}
							return next.map((x) =>
								x.id === wsId ? { ...x, streaming: false, content } : x,
							);
						}
						if (existing) {
							return next.map((x) =>
								x.id === wsId ? { ...x, content, streaming: true } : x,
							);
						}
						return insertAgentMessage(
							next,
							newToolMessage({
								id: wsId,
								stepNumber: data.stepNumber,
								toolName: 'web_search',
								time: new Date().toLocaleTimeString(),
								content,
								streaming: true,
							}),
						);
					});
				},
				'agent:supplement': (event) => {
					// Human steering/follow-up: mark the matching user bubble as
					// received. Cross-session peer mail: insert an `agent` tool
					// card in-chat (no separate tab) so collaboration is visible.
					// Background action auto-wake: insert a compact `actions`
					// card so the conversation shows the resume bridge (not
					// only a toast / sudden model restart).
					const data = event.payload;
					const tid = data.sessionId;
					const ctx = (data.additionalContext || '').trim();
					if (!tid || !ctx) return;
					const source = data.injectSource;
					if (source === 'cross_session') {
						const cardId = `peer-mail-${data.stepNumber ?? 0}-${data.runId ?? 0}-${ctx.length}`;
						const content = JSON.stringify({
							operation: 'inbox',
							auto: true,
							text: ctx,
						});
						updateSessionMessages(tid, (m) => {
							if (m.some((x) => x.id === cardId || (x.toolName === 'agent' && x.content === content))) {
								return m;
							}
							return insertAgentMessage(
								m,
								newToolMessage({
									id: cardId,
									stepNumber: data.stepNumber ?? 0,
									toolName: 'agent',
									content,
									time: new Date().toLocaleTimeString(),
								}),
							);
						});
						return;
					}
					if (source === 'action_result') {
						const parsed = parseActionResultInject(ctx);
						const actionId = parsed?.action_id || 'unknown';
						const cardId = `action-result-${actionId}-${data.stepNumber ?? 0}-${data.runId ?? 0}`;
						const content = JSON.stringify(
							parsed || {
								operation: 'result_injected',
								action_id: actionId,
								status: 'completed',
								auto: true,
							},
						);
						updateSessionMessages(tid, (m) => {
							if (m.some((x) => x.id === cardId)) return m;
							return insertAgentMessage(
								m,
								newToolMessage({
									id: cardId,
									stepNumber: data.stepNumber ?? 0,
									toolName: 'actions',
									content,
									time: new Date().toLocaleTimeString(),
								}),
							);
						});
						return;
					}
					updateSessionMessages(tid, (m) => {
						let marked = false;
						const next = [...m];
						for (let i = next.length - 1; i >= 0; i--) {
							const x = next[i];
							if (
								x.role === 'user' &&
								!x.received &&
								(x.content || '').trim() === ctx
							) {
								// Injected into the turn: ✓ and release the
								// "keep agent UI above me" anchor so post-steer
								// output can land after this bubble.
								next[i] = { ...x, received: true, steering: false };
								marked = true;
								break;
							}
						}
						return marked ? next : m;
					});
				},
				'agent:action': (event) => {
					const data = event.payload;
					const tid = data.sessionId;
					// A tool action finalizes the step's streaming blocks:
					// apply queued chunks first so the finalize is complete.
					flushChunksNow();
					updateModelState('tool');
					// The event carries the minted `step-*` id — the same id the
					// step row is persisted under — so the live card and the
					// resume badge are one entity.
					const toolMsgId = data.stepId;
					const { reasoningId, thoughtId } = blockIdsOf(
						tid,
						data.stepNumber,
						data.runId,
					);
					if (reasoningId) pruneSeq(reasoningId);
					if (thoughtId) pruneSeq(thoughtId);
					if (data.silent) {
						// Silent tool: no card is shown, but the preceding text
						// must still be finalized so it is inserted immediately.
						updateSessionMessages(tid, (m) =>
							finalizeStreamBlocks(
									data.suppressStreamedThought ? dropStreamedThought(m, thoughtId) : m,
								reasoningId,
								thoughtId,
							),
						);
						return;
					}
					updateSessionMessages(tid, (m) => {
						// Finalize any streaming reasoning and thought blocks —
						// a tool action means the text/reasoning phase is over.
						// Finalized blocks drop straggler chunks that flush
						// out of the batcher after this event.
						const fixed = finalizeStreamBlocks(
								data.suppressStreamedThought ? dropStreamedThought(m, thoughtId) : m,
							reasoningId,
							thoughtId,
						);
						const existing = fixed.find((x) => x.id === toolMsgId);
						if (existing) return fixed;
						return insertAgentMessage(
							fixed,
							newToolMessage({
								id: toolMsgId,
								stepNumber: data.stepNumber,
								toolName: data.toolName,
								time: new Date().toLocaleTimeString(),
								streaming: true,
								toolArgs: data.input ?? null,
							}),
						);
					});
				},
				'agent:tool_output': (event) => {
					// Live stdout/stderr preview while a foreground tool runs.
					// Side-channel store — does not rewrite the transcript list.
					const data = event.payload || {};
					const toolMsgId = data.stepId;
					const output = typeof data.output === 'string' ? data.output : '';
					if (!toolMsgId) return;
					setToolOutputPreview(toolMsgId, output);
				},
				'agent:observation': (event) => {
					const data = event.payload;
					const tid = data.sessionId;
					const toolMsgId = data.stepId;
					if (data.silent) {
						// Empty inbox (and other silent tools): action may have
						// already inserted a streaming placeholder — remove it.
						clearToolOutputPreview(toolMsgId);
						if (toolMsgId) {
							updateSessionMessages(tid, (m) => m.filter((x) => x.id !== toolMsgId));
						}
						return;
					}
					flushChunksNow();
					updateModelState('streaming');
					// Same minted step id the matching `agent:action` carried,
					// so the placeholder fill and the final badge stay one card.
					clearToolOutputPreview(toolMsgId);
					const actionId = actionIdFromObservation(data.observation);
					updateSessionMessages(tid, (m) => {
						const idx = m.findIndex((x) => x.id === toolMsgId);
						const msg = newToolMessage({
							id: toolMsgId,
							stepNumber: data.stepNumber,
							toolName: data.toolName,
							content: data.observation,
							askOptions: data.askOptions || [],
							actionId,
						});
						if (idx >= 0) {
							// Preserve the fields set by the action handler (e.g. the
							// bubble's timestamp) — only overwrite the observation
							// content and related fields. Background shells keep
							// actionId so the card binds to actionStore live output.
							const next = [...m];
							next[idx] = { ...next[idx], ...msg, streaming: false };
							return next;
						}
						return insertAgentMessage(m, msg);
					});
				},
				}),
				...actionEventListeners({
					'action:finished': (event) => {
						// Persist terminal background output onto the tool card and
						// clear actionId so a later refreshActions() cannot revert
						// the card to the original "running" observation ack.
						finalizeBackgroundActionMessages(event.payload);
					},
				}),
				...appEventListeners({
				'confirm:requested': (event) => {
					const data = event.payload;
					// Security confirmations are modal and resolve by step id, so
					// requests from background (non-active) sessions must still be
					// surfaced — dropping them would leave the tool call waiting
					// forever. The dialog shows which session the operation belongs
					// to so an approval is never misattributed.
					//
					// Multiple pending requests are QUEUED and shown one at a time:
					// a batched ReAct step can fire several gated tool calls at once,
					// and every one must wait for its own user answer. Auto-rejecting
					// the visible dialog when a second request arrives for the same
					// session would silently deny the first operation the user never
					// got to choose on — every request has a live backend wait, so
					// there is no "moved on" case that needs a defensive denial.
					const tid = data.sessionId || '';
					const session = sessions.find((t) => t.id === tid);
					confirmQueue = [
						...confirmQueue,
						{
							stepId: data.stepId,
							toolName: data.toolName,
							sessionId: tid,
							sessionTitle: session?.title || (tid || ''),
							riskLevel: data.riskLevel || 'medium',
							params: data.params ?? null,
							permissionKey: data.permissionKey || data.toolName || '',
						},
					];
					showNextConfirm();
				},
				}),
				// Token usage / cost stats — emitted after every LLM step.
				...agentEventListeners({
				'agent:usage': (event) => {
					const d = event.payload;
					if (!d.sessionId) return;
					const prompt = d.promptTokens || 0;
					const completion = d.completionTokens || 0;
					const cached = d.cachedTokens || 0;
					const creation = d.cacheCreationTokens || 0;
					const miss = d.cacheMissTokens || 0;
					const total = coalesceTokenTotal(
						prompt,
						completion,
						d.totalTokens || 0,
						cached,
						creation,
						d.cacheAccounting || 'unknown',
					);
					const cumPrompt = d.cumulativePromptTokens || 0;
					const cumCompletion = d.cumulativeCompletionTokens || 0;
					const cumCached = d.cumulativeCachedTokens || 0;
					const cumCreation = d.cumulativeCacheCreationTokens || 0;
					const cumMiss = d.cumulativeCacheMissTokens || 0;
					updateSessionTokenStats(d.sessionId, {
						promptTokens: prompt,
						completionTokens: completion,
						totalTokens: total,
						cachedTokens: cached,
						cacheCreationTokens: creation,
						cacheMissTokens: miss,
						cacheAccounting: d.cacheAccounting || 'unknown',
						contextTokens: d.contextTokens || 0,
						cacheExclusive: !!d.cacheExclusive,
						cumulativePromptTokens: cumPrompt,
						cumulativeCompletionTokens: cumCompletion,
						cumulativeTotalTokens: coalesceTokenTotal(
							cumPrompt,
							cumCompletion,
							d.cumulativeTotalTokens || 0,
							cumCached,
							cumCreation,
						),
						cumulativeCachedTokens: cumCached,
						cumulativeCacheCreationTokens: cumCreation,
						cumulativeCacheMissTokens: cumMiss,
						costUsd: d.costUsd ?? null,
						cumulativeCostUsd: d.cumulativeCostUsd ?? null,
						contextWindow: d.contextWindow ?? null,
						model: d.model ?? null,
						// A real usage event supersedes any restored estimate.
						estimated: false,
						// A live event means the conversation is active again:
						// the widget switches back to the per-step context view.
						restored: false,
					});
					// Also append the per-call detail so tool-card token chips
					// (stepUsage) update live — previously they only appeared
					// after restoreSessionLlmUsage on resume/reopen.
					if (d.stepNumber != null) {
						appendSessionLlmUsage(d.sessionId, {
							step_number: d.stepNumber,
							role: d.role || undefined,
							model: d.model ?? null,
							prompt_tokens: prompt,
							completion_tokens: completion,
							total_tokens: total,
							cached_tokens: cached,
							cache_creation_tokens: creation,
							cache_miss_tokens: miss,
							cache_accounting: d.cacheAccounting || 'unknown',
							cache_diagnostics: d.cacheDiagnostics || undefined,
							cost_usd: d.costUsd ?? null,
							has_cost: !!d.hasCost,
							duration_ms: d.durationMs ?? null,
						});
					}
				},
				// Context compaction notice — summarize a portion of the history.
				'agent:compaction': (event) => {
					const d = event.payload;
					const before = formatTokenCount(d.tokensBefore || 0);
					const after = formatTokenCount(d.tokensAfter || 0);
					addNotification(`上下文压缩：${before} → ${after} tokens`, 'info', 2500);
				},
				}),
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

		await Promise.all([sessionsP, restoreP, readyP]);

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
			// Session lifecycle changes may have reaped background jobs (a session
			// ending cancels its jobs without terminal events): re-sync the
			// action board so the panel drops entries that no longer exist.
			// Same for reminders: fired ones are gone from the pending list.
			refreshActions();
		})().catch((e) => {
			addNotification(`加载会话列表失败: ${formatError(e)}`, 'error', 3000);
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
	// The agent's ask questions are "awaiting" only while the session is paused
	// for the user's reply. Clear that state whenever the session resumes (the
	// user answered — by quick reply, typing, or voice) or its turn ends
	// (completed/error), so the "等待你的回答" indicator doesn't linger on
	// answered or abandoned questions. The `resolved` label is cleared too:
	// once the session resumes, any locally-chosen quick-reply answer that was
	// NOT part of the submitted message (e.g. the user typed their own reply
	// instead) must not keep displaying as "已选择/已忽略" — the submitted
	// user bubble is the record of what was actually sent.
	/** @param {string} sessionId */
	function clearAskAwaiting(sessionId) {
		updateSessionMessages(sessionId, (m) =>
			m.map((x) => (x.type === 'ask' ? { ...x, awaiting: false, resolved: null } : x)),
		);
		// A resume/end also invalidates any locally-chosen quick-reply answers
		// for the pending batch, so a later batch never inherits stale ones.
		resolvedAskIds.delete(sessionId);
		clearAskSelections(sessionId);
	}

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
			addNotification(`发送失败: ${formatError(e)}`, 'error', 5000);
		}
	}

	// Selected ask option chips per session (msgId -> selected labels). Click
	// toggles selection; Enter in the input box submits (see handleInputSubmit).
	/** @type {Map<string, Map<string, string[]>>} */
	let askSelections = new Map();

	/** @param {string} sessionId */
	function clearAskSelections(sessionId) {
		askSelections.delete(sessionId);
		askSelectionsReady = computeAskSelectionsReady();
	}

	/** @param {string} msgId @param {string[]} selected */
	function handleAskSelectionChange(msgId, selected) {
		if (!activeSessionId || !msgId) return;
		const byMsg = askSelections.get(activeSessionId) || new Map();
		if (!selected || selected.length === 0) byMsg.delete(msgId);
		else byMsg.set(msgId, [...selected]);
		if (byMsg.size === 0) askSelections.delete(activeSessionId);
		else askSelections.set(activeSessionId, byMsg);
		askSelectionsReady = computeAskSelectionsReady();
	}

	// True when every currently awaiting ask card has at least one selected
	// option — InputRouter then allows Enter with an empty draft.
	let askSelectionsReady = $state(false);

	function computeAskSelectionsReady() {
		if (!activeSessionId) return false;
		const awaiting = (get(sessionMessagesStore)[activeSessionId] || []).filter(
			(x) => x.type === 'ask' && x.awaiting,
		);
		if (awaiting.length === 0) return false;
		const byMsg = askSelections.get(activeSessionId);
		if (!byMsg) return false;
		return awaiting.every((x) => (byMsg.get(x.id) || []).length > 0);
	}

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

	// Enter pressed on a focused ask option chip (after clicking it, the
	// button keeps focus) must submit the composed answers instead of the
	// native button Enter behavior, which re-triggers the chip click and
	// toggles the selection off. Empty payload: the page composes the chips.
	function handleAskSubmit() {
		if (!activeSessionId) return;
		autoFollow = true;
		trySubmitAskSelections(activeSessionId, '', [], []);
	}

	/**
	 * @param {string} sessionId
	 * @param {string} extraText
	 * @param {any} images
	 * @param {any} files
	 */
	function trySubmitAskSelections(sessionId, extraText, images, files) {
		const awaiting = (get(sessionMessagesStore)[sessionId] || []).filter(
			(x) => x.type === 'ask' && x.awaiting,
		);
		if (awaiting.length === 0) return false;
		const byMsg = askSelections.get(sessionId);
		if (!byMsg) return false;
		if (!awaiting.every((x) => (byMsg.get(x.id) || []).length > 0)) return false;
		for (const ask of awaiting) {
			const selected = byMsg.get(ask.id) || [];
			resolveAsk(ask.id, { answer: selected.join(' ') }, { deferSubmit: true });
		}
		const submitted = resolvedAskIds.get(sessionId);
		resolvedAskIds.delete(sessionId);
		clearAskSelections(sessionId);
		submitActionAnswers(sessionId, submitted, extraText, images, files);
		return true;
	}

	// Quick-reply answers / ignores chosen for the CURRENT batch of pending
	// ask questions, per session. When the agent asks several questions in one
	// batch (multiple `ask` calls in a single step), the session must stay
	// paused until every question is resolved — answering only one would
	// resume the session and silently discard the others. Once all are answered
	// or ignored, a single composed reply is submitted. Typing a message in
	// the input box bypasses this and resumes immediately (unless every ask
	// already has selected options — then Enter merges selections + text).
	let resolvedAskIds = new Map(); // sessionId -> Set<msgId>

	// Mark one pending ask card as resolved (answered via option chips or
	// ignored) and submit the composed answers once the batch is complete.
	/** @param {string} msgId @param {any} resolved @param {{ deferSubmit?: boolean }} [opts] */
	function resolveAsk(msgId, resolved, opts = {}) {
		if (!activeSessionId || !msgId) return;
		const ids = resolvedAskIds.get(activeSessionId) || new Set();
		// Re-entry guard: a double-click / queued click on the same card (the
		// DOM may not have re-rendered yet) must not compose and submit the
		// same answer twice.
		if (ids.has(msgId)) return;
		updateSessionMessages(activeSessionId, (m) =>
			m.map((x) =>
				x.id === msgId && x.type === 'ask' && !x.resolved
					? { ...x, awaiting: false, resolved }
					: x,
			),
		);
		ids.add(msgId);
		resolvedAskIds.set(activeSessionId, ids);
		const byMsg = askSelections.get(activeSessionId);
		if (byMsg) {
			byMsg.delete(msgId);
			if (byMsg.size === 0) askSelections.delete(activeSessionId);
		}
		askSelectionsReady = computeAskSelectionsReady();
		if (opts.deferSubmit) return;
		const remainingMessages = (get(sessionMessagesStore)[activeSessionId] || []).filter(
			(x) => x.type === 'ask' && x.awaiting,
		);
		if (remainingMessages.length === 0) {
			const submitted = resolvedAskIds.get(activeSessionId);
			resolvedAskIds.delete(activeSessionId);
			submitActionAnswers(activeSessionId, submitted);
		}
	}

	// Compose all answers chosen for the resolved batch into a single user
	// message and deliver it, which resumes the paused session. A single
	// question keeps the raw answer; multiple questions quote each one so the
	// model can map answers back to its questions. Ignored questions are
	// marked as 忽略. Optional typed text / attachments from the input box
	// are appended when Enter submitted selected chips.
	/** @param {string} sessionId @param {any} resolvedIds @param {string} [extraText] @param {any} [images] @param {any} [files] */
	function submitActionAnswers(sessionId, resolvedIds, extraText = '', images = [], files = []) {
		if (!resolvedIds || resolvedIds.size === 0) return;
		const asks = (get(sessionMessagesStore)[sessionId] || []).filter(
			(x) => x.type === 'ask' && x.resolved && resolvedIds.has(x.id),
		);
		if (asks.length === 0) return;
		const single = asks.length === 1;
		let text = asks
			.map((x, i) => {
				const answer = x.resolved.ignored ? '忽略' : x.resolved.answer;
				return single ? answer : `关于「${x.content || `问题 ${i + 1}`}」：${answer}`;
			})
			.join('\n');
		const extra = (extraText || '').trim();
		if (extra) text = text ? `${text} ${extra}` : extra;
		autoFollow = true;
		submitMessage(text, images, files);
	}

	// The user chooses not to answer a pending question; counting as a
	// resolution so the batch can resume once all questions are handled.
	/** @param {string} msgId */
	function handleIgnoreAsk(msgId) {
		if (!activeSessionId) return;
		resolveAsk(msgId, { ignored: true });
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
		const resolvedScope =
			scope || (trustSession ? 'session' : 'once');
		try {
			await invoke('resolve_confirmation', {
				stepId: resolvedStep,
				confirmed: approved,
				trustSession: resolvedScope === 'session' && resolvedEffect === 'allow',
				effect: resolvedEffect,
				scope: resolvedScope,
			});
		} catch (e) {
			addNotification(`确认失败: ${formatError(e)}`, 'error', 3000);
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
			return '等待后台';
		return isPausedStatus(session.status) ? '已暂停' : '等待中';
	}
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
				rollbackDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' };
		}}
	/>

	<ContextMenu
		open={ctxMenu.open}
		x={ctxMenu.x}
		y={ctxMenu.y}
		items={ctxMenuItems}
		onClose={closeCtxMenu}
	/>

	<div class="messages-wrap">
		<div class="messages-area" bind:this={messagesEl} onscroll={onScroll}>
			{#if messages.length === 0}
				<div class="welcome" in:fly={{ y: 12, duration: 330 }}>
					<Logo size={48} />
					<h2>Haven</h2>
					<p>按 {hotkeyBinding} 开始录音，或直接输入指令</p>
				</div>
			{:else}
				<div class="message-list">
					{#each messages as msg (msg.id)}
						<ChatBubble
							role={msg.role}
							content={msg.content}
							type={msg.type}
							voice={msg.voice}
							time={msg.time}
							streaming={!!msg.streaming}
							toolName={msg.toolName ?? ''}
							messageId={msg.id}
							stepNumber={msg.stepNumber}
							usage={msg.type === 'tool' ? stepUsage(msg.stepNumber) : null}
							toolArgs={msg.toolArgs ?? null}
							attachments={msg.attachments}
							options={msg.options ?? []}
							awaiting={msg.awaiting ?? false}
							received={msg.received ?? false}
							resolved={msg.resolved ?? null}
							actionId={msg.actionId ?? null}
							onContextMenu={handleContextMenu}
							onAskSelectionChange={handleAskSelectionChange}
							onIgnore={handleIgnoreAsk}
							onAskSubmit={handleAskSubmit}
						/>
					{/each}
				</div>
			{/if}
			{#if awaitingBackground && !activeSessionError}
				<div class="awaiting-bg-banner" in:fly={{ y: 8, duration: 300 }} role="status">
					<span class="awaiting-bg-dot" aria-hidden="true"></span>
					<span class="awaiting-bg-text">
						等待后台任务结果{#if awaitingBackgroundCount > 1}（{awaitingBackgroundCount}）{/if}，完成后将自动继续
					</span>
				</div>
			{/if}
			{#if activeSessionError}
				<div class="continue-banner" in:fly={{ y: 8, duration: 300 }}>
					<button
						class="md-btn md-btn--filled continue-btn"
						onclick={handleContinue}
						type="button"
					>
						<svg
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"><polygon points="5 3 19 12 5 21 5 3" /></svg
						>
						继续生成
					</button>
				</div>
			{/if}
		</div>
		{#if !autoFollow && messages.length > 0}
			<button
				class="jump-bottom"
				onclick={jumpToBottom}
				aria-label="返回底部"
				title="返回底部"
				type="button"
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
			</button>
		{/if}
	</div>

	<InputRouter
		bind:this={inputRouterRef}
		{activeSessionId}
		{hotkeyBinding}
		{isGenerating}
		{sessionRunning}
		allowEmptySubmit={askSelectionsReady}
		{...inputLimits}
		onsubmit={handleInputSubmit}
		onstop={endSession}
	>
		{#snippet toolbarLeft()}
			<SessionToolbar
				{activeSessionId}
				{showSessionMenu}
				{sessionMenuOpen}
				{parallelSessions}
				{menuSessions}
				onToggleSessionMenu={() => {
					if (showSessionMenu) sessionMenuOpen = !sessionMenuOpen;
					else newSession();
				}}
				onNewSession={newSession}
				onSwitchSession={switchToSession}
				onEndSession={endSession}
				messagesLength={messages.length}
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
	</InputRouter>
</div>

<style>
	.chat-page {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}
	.messages-wrap {
		position: relative;
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		/* Chat content has its own narrower reading-friendly cap; the
		 * layout shell handles the wider-page case so we only need to
		 * keep messages from getting too narrow on small viewports. */
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}
	.messages-area {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: var(--md-sys-space-md);
	}
	.jump-bottom {
		position: absolute;
		right: var(--md-sys-space-md);
		bottom: var(--md-sys-space-sm);
		width: 36px;
		height: 36px;
		border: none;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-2);
		transition: background var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
		z-index: 5;
	}
	.jump-bottom:hover {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.welcome {
		text-align: center;
		padding: var(--md-sys-space-4xl) 0 var(--md-sys-space-3xl);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.welcome h2 {
		font-family: var(--md-ref-typeface-brand);
		font-size: 32px;
		font-weight: 700;
		letter-spacing: 0.5px;
		color: var(--md-sys-color-primary);
	}
	.welcome p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-size, 14px);
		max-width: 420px;
	}
	.message-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}

	.continue-banner {
		display: flex;
		align-items: center;
		justify-content: flex-start;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}
	.continue-btn {
		gap: var(--md-sys-space-xs);
		font-size: 13px;
	}

	.awaiting-bg-banner {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: clamp(600px, 92vw, 800px);
		margin: var(--md-sys-space-sm) auto 0;
		width: 100%;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.awaiting-bg-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-tertiary, #7c9cff);
		flex-shrink: 0;
		animation: awaiting-bg-pulse 1.2s ease-in-out infinite;
	}
	.awaiting-bg-text {
		line-height: 1.4;
	}
	@keyframes awaiting-bg-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.9);
		}
		50% {
			opacity: 1;
			transform: scale(1);
		}
	}
</style>
