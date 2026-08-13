<script module>
	// Per-session cache for the toolbar model switcher's model discovery.
	// Dev-mode page reloads (Vite HMR reconnect, window re-show, single-
	// instance re-entry) remount the chat view and would otherwise fire a
	// duplicate discover_models request against the same default endpoint.
	// Cache the result per base URL and share in-flight requests so reloads
	// reuse the list instead of re-hitting the provider's /models endpoint.
	const defaultModelsCache = {
		baseUrl: null,
		list: null,
		inflight: null,
	};
</script>

<script>
	import logger from '$lib/logger.js';
	import { buildReviewMessages, mergeLiveStreaming } from '$lib/reviewMessages.js';
	import {
		accumulateStreamChunk,
		applyThoughtSnap,
		stepId,
		toolId,
		finalizeStreamBlocks,
		newToolMessage,
	} from '$lib/streaming.js';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { fly } from 'svelte/transition';
	import { get } from 'svelte/store';
	import { invoke } from '$lib/tauri.js';
	import { registerListeners } from '$lib/events.js';
	import {
		sessionMessagesStore,
		sessionStore,
		addNotification,
		updateSessionMessages,
		adoptDraftMessages,
		clearSessionMessages,
		clearSeqMap,
		reviewTargetStore,
		activeSessionIdStore,
		sessionTokenStatsStore,
		updateSessionTokenStats,
		clearSessionTokenStats,
		restoreSessionTokenStats,
		sessionLlmUsageStore,
		restoreSessionLlmUsage,
		clearSessionLlmUsage,
		formatTokenCount,
		formatCostUsd,
		seqLastSeen,
		pruneSeq,
		updateModelState,
		modelStateStore,
		refreshTasks,
		DRAFT_KEY,
		NEW_TASK_INTENT_KEY,
		newSessionIntentStore,
	} from '$lib/stores.js';
	import { submitTranscript } from '$lib/submit.js';
	import { syncStore, syncStoreImmediate } from '$lib/syncStore.js';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import RollbackDialog from '$lib/RollbackDialog.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import Logo from '$lib/Logo.svelte';
	import InputRouter from '$lib/InputRouter.svelte';

	let inputRouterRef = $state(null);

	// Attachment & compression limits for the input router, loaded from the
	// persisted [context_limits] config (editable on the settings "输入格式"
	// page). Defaults mirror the backend config until settings arrive.
	let inputLimits = $state({
		maxImages: 4,
		maxImageBytes: 10 * 1024 * 1024,
		maxImageDim: 1568,
		jpegQuality: 0.85,
		maxFiles: 5,
		maxFileBytes: 20 * 1024 * 1024,
	});
	let messages = $state([]);
	let sessions = $state([]);
	let confirmDialog = $state({
		stepId: null,
		toolName: '',
		sessionId: '',
		sessionTitle: '',
		riskLevel: 'medium',
	});
	let activeSessionId = $state(get(activeSessionIdStore));
	let rollbackDialog = $state({ open: false, stepNumber: null, role: '', content: '', msgId: '' });
	let rollbackLoading = $state(false);

	// Model switcher state: the registry catalog plus the current default
	// model name, displayed on the toolbar button and filtered in the menu.
	let modelMenuOpen = $state(false);
	let sessionMenuOpen = $state(false);
	let modelOptions = $state([]);
	let currentModelName = $state('');
	let currentModelId = $state('');
	let currentEffort = $state('');
	// Provider built-in web search mode ("off" | "auto" | "always").
	// Defaults to off (opt-in); "auto" lets the model decide when to search.
	let currentWebSearch = $state('off');
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
	 * @property {number} cumulativePromptTokens
	 * @property {number} cumulativeCompletionTokens
	 * @property {number} cumulativeTotalTokens
	 * @property {number|null} costUsd
	 * @property {number|null} cumulativeCostUsd
	 * @property {number|null} contextWindow
	 * @property {string|null} model
	 * @property {boolean} [estimated] - totals restored from a rough backend
	 *   estimate (session predates usage persistence), not real recorded usage.
	 * @property {boolean} [restored] - entry came from persistence (review /
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
	// persisted `llm_usage` when a review conversation opens). Used to render
	// per-step token chips on tool cards and the widget's per-call tooltip.
	/** @type {Array<object>} */
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

	/**
	 * Aggregate the persisted usage-detail rows for one ReAct step (a step
	 * can carry more than one call when a compaction retry re-ran it). Returns
	 * null when the step has no recorded detail (or the session predates per-call
	 * persistence). Memoized per step: the each-block calls this for every
	 * tool bubble on every streaming flush, and a fresh object per call would
	 * churn child component updates across the whole long conversation.
	 * @param {number|null} stepNumber
	 * @returns {{prompt: number, completion: number, total: number, cost: number, hasCost: boolean, durationMs: number, model: string|null, calls: number}|null}
	 */
	const stepUsageCache = new Map();
	function stepUsage(stepNumber) {
		if (stepNumber == null || llmUsage.length === 0) return null;
		const cached = stepUsageCache.get(stepNumber);
		if (cached !== undefined) return cached;
		const calls = llmUsage.filter((u) => u.step_number === stepNumber);
		if (calls.length === 0) return null;
		const value = {
			prompt: calls.reduce((s, u) => s + (u.prompt_tokens || 0), 0),
			completion: calls.reduce((s, u) => s + (u.completion_tokens || 0), 0),
			total: calls.reduce((s, u) => s + (u.total_tokens || 0), 0),
			cost: calls.reduce((s, u) => s + (u.cost_usd || 0), 0),
			hasCost: calls.some((u) => u.has_cost),
			durationMs: calls.reduce((s, u) => s + (u.duration_ms || 0), 0),
			model: calls.map((u) => u.model).filter(Boolean).at(-1) || null,
			calls: calls.length,
		};
		stepUsageCache.set(stepNumber, value);
		return value;
	}

	/**
	 * Context-window utilization for the active session. Returns
	 * `{ used, window, ratio }` where `used` is the last reported prompt
	 * token count (per-step, not cumulative) and `window` is the model's
	 * configured budget. Returns `null` when no data is available.
	 */
	const contextBudget = $derived.by(() => {
		if (!tokenStats) return null;
		const window = tokenStats.contextWindow || 0;
		const used = tokenStats.promptTokens || 0;
		if (!window) return null;
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
			sessions.some(
				(t) => t.id === activeSessionId && (t.status === 'running' || t.status === 'pending'),
			),
	);
	// Tooltip for the idle token widget. While the active session is still
	// running (streaming, tool-calling, or queued) more `agent:usage`
	// events are expected, so "waiting" is accurate. A finished or
	// history-opened conversation with no persisted usage will never
	// receive events — show a neutral hint instead of waiting forever.
	const tokenStatsHint = $derived(isGenerating || sessionRunning ? '等待 LLM 统计' : '暂无统计');
	// Sessions executing in parallel (running or waiting). When 2+ exist, the
	// new-session button turns into a switcher menu: switch to a parallel session
	// or start a new one. Otherwise the button keeps its default behavior.
	const parallelSessions = $derived(
		sessions.filter((t) => t.status === 'running' || t.status === 'pending'),
	);
	// Menu source: parallel sessions plus paused ones — a paused session is
	// otherwise invisible in the chat view (its conversation is not shown).
	const menuSessions = $derived(
		sessions.filter((t) =>
			['running', 'pending', 'paused'].includes(t.status),
		),
	);
	const showSessionMenu = $derived(menuSessions.length >= 2);

	function buildTokenTooltip(/** @type {SessionTokenStats} */ s) {
		const parts = [];
		if (s.restored) {
			parts.push(
				`累计上传 ${s.cumulativePromptTokens || 0} → 累计生成 ${s.cumulativeCompletionTokens || 0} tokens`,
			);
			parts.push(`累计 ${s.cumulativeTotalTokens || 0} tokens`);
		} else {
			parts.push(
				`上传 ${s.promptTokens || 0} → 生成 ${s.completionTokens || 0} tokens`,
			);
			parts.push(`累计 ${s.cumulativeTotalTokens || 0} tokens`);
			if (s.cumulativePromptTokens != null)
				parts.push(
					`累计上传 ${s.cumulativePromptTokens} → 累计生成 ${s.cumulativeCompletionTokens} tokens`,
				);
		}
		if (s.model) parts.push(`模型 ${s.model}`);
		if (s.contextWindow) {
			const pct =
				s.promptTokens && s.contextWindow
					? `${((s.promptTokens / s.contextWindow) * 100).toFixed(0)}%`
					: '?';
			parts.push(`上下文 ${pct} / ${formatTokenCount(s.contextWindow)}`);
		}
		if (s.cumulativeCostUsd != null) parts.push(`费用 ${formatCostUsd(s.cumulativeCostUsd)}`);
		if (s.estimated) parts.push('估算值（历史对话，未计费）');
		// Per-call breakdown from the persisted llm_usage detail (cap the
		// list so a long session doesn't produce an unwieldy tooltip).
		if (llmUsage.length > 0) {
			parts.push(`— 每次调用 —`);
			const shown = llmUsage.slice(-10);
			for (const u of shown) {
				const where = u.step_number != null ? `第${u.step_number}步` : '—';
				let line = `${where} ${u.total_tokens || 0} tokens`;
				if (u.prompt_tokens != null) {
					line += ` (↑${u.prompt_tokens}→↓${u.completion_tokens || 0})`;
				}
				if (u.model) line += ` ${u.model}`;
				if (u.duration_ms != null && u.duration_ms > 0) {
					line += ` ${(u.duration_ms / 1000).toFixed(1)}s`;
				}
				if (u.has_cost && u.cost_usd != null) line += ` ${formatCostUsd(u.cost_usd)}`;
				parts.push(line);
			}
			if (llmUsage.length > shown.length) {
				parts.push(`…共 ${llmUsage.length} 次调用`);
			}
		}
		return parts.join('\n');
	}

	const effortOptions = [
		{ value: '', label: '默认' },
		{ value: 'low', label: '低' },
		{ value: 'medium', label: '中' },
		{ value: 'high', label: '高' },
	];

	const webSearchOptions = [
		{ value: 'off', label: '关闭' },
		{ value: 'auto', label: '自动' },
		{ value: 'always', label: '总是' },
	];

	async function handleWebSearchSelect(value) {
		const label = webSearchOptions.find((o) => o.value === value)?.label || '关闭';
		try {
			await invoke('set_web_search', { role: 'default_model', mode: value });
			currentWebSearch = value;
			addNotification(`联网搜索: ${label}`, 'success', 2500);
		} catch (e) {
			addNotification(`设置联网搜索失败: ${e}`, 'error', 4000);
		}
	}

	async function handleModelSelect(m) {
		modelMenuOpen = false;
		try {
			await invoke('switch_model', { role: 'default_model', modelId: m.id });
			currentModelId = m.id;
			currentModelName = m.name || m.id;
			addNotification(`已切换默认模型: ${currentModelName}`, 'success', 3000);
		} catch (e) {
			addNotification(`切换模型失败: ${e}`, 'error', 4000);
		}
	}

	async function handleEffortSelect(value) {
		const label = effortOptions.find((o) => o.value === value)?.label || '默认';
		try {
			await invoke('set_reasoning_effort', { role: 'default_model', effort: value || null });
			currentEffort = value || '';
			addNotification(`思考强度: ${label}`, 'success', 2500);
		} catch (e) {
			addNotification(`设置思考强度失败: ${e}`, 'error', 4000);
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

	function handleWindowClick(e) {
		if (modelMenuOpen) {
			const menu = document.querySelector('.model-menu');
			const btn = document.querySelector('.model-switch-btn');
			if (menu && btn && !menu.contains(e.target) && !btn.contains(e.target)) {
				modelMenuOpen = false;
			}
		}
		if (sessionMenuOpen) {
			const menu = document.querySelector('.session-menu');
			const btn = document.querySelector('.session-switch-btn');
			if (menu && btn && !menu.contains(e.target) && !btn.contains(e.target)) {
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
		rollbackLoading = true;
		try {
			if (role === 'user') {
				// User-message rollback: pause the session and put the message
				// text back in the input box so the user can edit and re-send.
				// The backend resolves targetMessageId against persisted session
				// messages and errors when the id does not match (no more
				// content-based guessing).
				await invoke('rollback_session', {
					sessionId: activeSessionId,
					targetStep: stepNumber,
					pause: true,
					targetMessageId: msgId,
				});
				clearSeqMap(activeSessionId);
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
				clearSeqMap(activeSessionId);
				await resyncSessionMessages(activeSessionId);
				addNotification(`已回退到第 ${stepNumber} 步`, 'info', 3000);
			}
		} catch (e) {
			addNotification(`回退失败: ${e}`, 'error', 5000);
		}
		rollbackLoading = false;
		rollbackDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' };
		await loadSessions();
	}

	// Rebuild a session's in-memory message list from the authoritative DB
	// state. Used after rollback (and by handleContinue) so the UI cannot
	// diverge from what the backend actually kept/deleted.
	async function resyncSessionMessages(sessionId) {
		if (!sessionId) return;
		try {
			const result = await invoke('get_session_for_review', { sessionId });
			const dbMessages = buildReviewMessages(result);
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
			addNotification(`同步消息失败: ${e}`, 'error', 3000);
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
		// session (submit.js) or they explicitly switch to another session. Also
		// persisted to localStorage so the next app launch skips restoring the
		// previous conversation.
		newSessionIntentStore.set(true);
		if (browser) localStorage.setItem(NEW_TASK_INTENT_KEY, '1');
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
	function evictTerminalSessionMemory(sessionId) {
		if (!sessionId || (activeSessionId && sessionId === activeSessionId)) return;
		clearSessionMessages(sessionId);
		clearSessionTokenStats(sessionId);
		clearSessionLlmUsage(sessionId);
		clearSeqMap(sessionId);
	}

	async function switchToSession(sessionId) {
		sessionMenuOpen = false;
		// The previously active session is about to be deactivated: if it is
		// already terminal (completed/error — it never evicted while it was
		// being watched), reclaim its memory now; switchToSession below reloads
		// from the DB when it is re-opened.
		const prevActive = activeSessionId;
		if (prevActive && prevActive !== sessionId) {
			const prevSession = sessions.find((x) => x.id === prevActive);
			if (prevSession && (prevSession.status === 'completed' || prevSession.status === 'error')) {
				evictTerminalSessionMemory(prevActive);
			}
		}
		try {
			const result = await invoke('get_session_for_review', { sessionId });
			const dbMessages = buildReviewMessages(result);
			// Drop DB step badges for steps already represented by a live
			// tool card (the running session may be mid-step): the live card
			// keeps streaming its observation. Their ids differ, so plain
			// id dedup would leave both visible.
			updateSessionMessages(sessionId, (existing) =>
				mergeLiveStreaming(dbMessages, existing, { dropToolSteps: true }),
			);
			restoreSessionTokenStats(sessionId, result.usage, result.usage_estimated);
			restoreSessionLlmUsage(sessionId, result.llm_usage);
			// An explicit switch abandons the fresh-start intent: the chosen
			// session becomes the active conversation (and may be auto-restored
			// on the next app launch).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_TASK_INTENT_KEY);
			activeSessionId = sessionId;
			activeSessionIdStore.set(sessionId);
			const t = sessions.find((x) => x.id === sessionId);
			addNotification(`已切换到：${t?.title || '会话'}`, 'info', 1500);
		} catch (e) {
			addNotification(`切换会话失败: ${e}`, 'error', 4000);
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
		} catch (e) {
			addNotification(`结束会话失败: ${e}`, 'error', 3000);
		}
		activeSessionId = null;
		activeSessionIdStore.set(null);
		newSessionIntentStore.set(false);
	}

	async function handleContinue() {
		if (!activeSessionId) return;
		const tid = activeSessionId;
		// Capture the ids of the trailing assistant messages BEFORE invoking
		// continue_session. These are the partial outputs from the interrupted
		// step that the backend will delete from the DB. We must remove them
		// from the UI too, but only these — the dispatcher may start the retry
		// before this function resumes and append NEW assistant messages
		// (different run_id in their ids) that must NOT be dropped.
		const currentMessages = get(sessionMessagesStore)[tid] || [];
		let trailingIdx = currentMessages.length;
		while (trailingIdx > 0 && currentMessages[trailingIdx - 1].role === 'assistant') {
			trailingIdx--;
		}
		// continue_session truncates the interrupted step's partial output from
		// the DB for a clean retry, so those trailing assistant messages would
		// otherwise be dropped on resync. But the partial REASONING ("Thinking…")
		// already streamed before the error is valuable context and must not
		// vanish — keep it so the user keeps seeing the thinking they watched.
		// Only the partial thought (final answer text) and stale tool/final
		// messages are dropped: the backend truncates them and the retry
		// regenerates them.
		const partialIds = new Set(
			currentMessages
				.slice(trailingIdx)
				.filter((m) => m.type !== 'reasoning')
				.map((m) => m.id),
		);
		try {
			// First unblock the errored session: continue_session truncates the
			// partial output and sets the session to Pending so the "继续" user
			// message below is accepted instead of being dropped as a
			// terminal-state supplement.
			await invoke('continue_session', { sessionId: tid });
			sessionErrorId = null;
			activeSessionError = false;
			// Re-sync from the authoritative post-truncate DB state instead of
			// guessing which trailing messages to drop. Every streamed message
			// (reasoning/thought/tool/final) carries role 'assistant', so a
			// naive "drop trailing assistants" sweep would clear completed tool
			// cards + observations that continue_session actually KEEPS — it only
			// truncates the interrupted final answer. Rebuilding from the DB
			// reproduces that exactly. Any NEW retry streaming that arrived
			// during the await is merged in on top; the old captured partials
			// are dropped from the existing store so they aren't re-added.
			try {
				const result = await invoke('get_session_for_review', { sessionId: tid });
				updateSessionMessages(tid, (existing) => {
					const dbMessages = buildReviewMessages(result);
					const keptExisting = existing.filter((m) => !partialIds.has(m.id));
					return mergeLiveStreaming(dbMessages, keptExisting);
				});
			} catch (e) {
				// Fallback: if the resync fails, at least drop the captured
				// partials so the stale interrupted output is removed.
				if (partialIds.size > 0) {
					updateSessionMessages(tid, (m) => {
						const filtered = m.filter((x) => !partialIds.has(x.id));
						return filtered.length !== m.length ? filtered : m;
					});
				}
			}
			clearSeqMap(tid);
			// Continue by sending a real user message, so "继续" appears in
			// the conversation and is delivered to the agent as an
			// interjection, just like a typed or quick-reply message.
			autoFollow = true;
			submitMessage('继续', []);
			await loadSessions();
		} catch (e) {
			addNotification(`继续失败: ${e}`, 'error', 5000);
			// Keep the banner visible so the user can retry.
		}
	}

	// Tauri event listener handle (registered in onMount, disposed in
	// onDestroy). See eventRegistrations below.
	let eventRegistrations = null;
	let messagesEl;
	let autoFollow = $state(true);
	let scrollRafPending = false;
	let dead = false;
	// Guards concurrent loadSessions() calls so a stale response can't overwrite
	// a newer one.
	let loadSessionsSeq = 0;

	// Sync the Svelte store to a $state variable — $effect does NOT track
	// get(store), so we must use .subscribe() to get reactive updates.
	// Also read the current value once on mount via get(), otherwise values
	// set before subscription (e.g. by history review) are never received.
	let sessionMessagesDict = $state({});
	$effect(() =>
		syncStoreImmediate(
			sessionMessagesStore,
			(v) => {
				sessionMessagesDict = v;
			},
			get,
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
	let sessionErrorId = $state(null);

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
	// above mirrors state → store only; submit.js writes the store directly
	// when a submission creates a fresh session (its `SessionCreated` result never
	// passes through this page), and the view must follow the new session
	// instead of staying on the blank draft. Guarded with `!activeSessionId`
	// (never override a session the user is actively viewing) AND the
	// fresh-start intent (while the intent is pending, a background session
	// creation must not hijack the blank draft — the submission that
	// fulfills the intent clears it before writing the store).
	$effect(() =>
		syncStore(activeSessionIdStore, (id) => {
			if (id && !activeSessionId && !get(newSessionIntentStore)) activeSessionId = id;
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
	// review, app-start auto-restore): at that moment every bubble is
	// content-visibility-skipped and reports only its contain-intrinsic-size
	// estimate (~120px), so the first scrollToBottom lands above the real
	// bottom. Force one full render pass — the real sizes are then remembered
	// by `contain-intrinsic-size: auto` — scroll, and restore lazy rendering.
	function scrollToBottomAfterOpen() {
		if (dead || !messagesEl) return;
		const list = messagesEl.querySelector('.message-list');
		if (!list) return;
		const bubbles = list.querySelectorAll('.bubble');
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

	// Streaming chunks are coalesced to ONE store flush per animation frame:
	// a burst of chunk events (long answers, parallel streams) is applied to
	// the message list in a single update, so the webview re-renders at most
	// once per frame no matter how many chunks arrive. Without this, a fast
	// stream saturates the webview main thread (every chunk re-renders the
	// conversation), the Tauri IPC channel backs up, the backend's event
	// buffer overflows and drops — and streaming visibly dies ("nothing, then
	// a big dump"). Events that must see the flushed state (agent:thought
	// snap, agent:action, agent:observation) flush synchronously first.
	const pendingChunks = [];
	let chunkFlushRaf = 0;
	// Hard cap on queued chunks: if the webview is hidden/occluded,
	// requestAnimationFrame can stall indefinitely, so an unbounded queue
	// would grow for the whole stream. On overflow the OLDEST queued chunk is
	// dropped — chunks are self-healing (the step's final snap/full-text
	// reconcile replaces accumulated deltas), so evicting old ones loses
	// nothing authoritative, mirroring the backend's event-buffer policy.
	const PENDING_CHUNK_MAX = 2000;
	let pendingChunkDrops = 0;

	function flushPendingChunks() {
		chunkFlushRaf = 0;
		if (pendingChunks.length === 0) return;
		const batch = pendingChunks.splice(0);
		// Merge deltas per step before touching the message list: each
		// accumulateStreamChunk call copies the whole conversation array, so
		// applying N chunks of the same step separately costs O(N × list) per
		// flush — a long answer on a long conversation stalls the main thread.
		// Concatenating the deltas preserves the final text (the step's snap
		// reconciles everything anyway) and collapses the work to O(steps ×
		// list), normally one array copy per flush.
		const mergedBySid = new Map();
		for (const c of batch) {
			const prev = mergedBySid.get(c.sid);
			if (prev) {
				prev.delta = (prev.delta || '') + (c.delta || '');
				prev.finalizeReasoning = prev.finalizeReasoning || c.finalizeReasoning;
			} else {
				mergedBySid.set(c.sid, { ...c });
			}
		}
		// Group by session preserving arrival order within each session.
		const bySession = new Map();
		for (const c of mergedBySid.values()) {
			let list = bySession.get(c.tid);
			if (!list) bySession.set(c.tid, (list = []));
			list.push(c);
		}
		for (const [tid, chunks] of bySession) {
			updateSessionMessages(tid, (m) => {
				let next = m;
				for (const c of chunks) {
					if (c.finalizeReasoning) {
						const reasoningId = stepId('reasoning', c.tid, c.stepNumber, c.runId);
						const rIdx = next.findIndex((x) => x.id === reasoningId && x.streaming);
						if (rIdx >= 0) {
							next = next.map((x) =>
								x.id === reasoningId ? { ...x, streaming: false } : x,
							);
							pruneSeq(reasoningId);
						}
					}
					if (c.delta) {
						next = accumulateStreamChunk(next, {
							stepId: c.sid,
							stepIdPrefix: c.stepIdPrefix,
							delta: c.delta,
							msgType: c.msgType,
							stepNumber: c.stepNumber,
							time: c.time,
						});
					}
				}
				return next;
			});
		}
	}

	/** Flush pending chunks synchronously (before snap/action/observation). */
	function flushChunksNow() {
		if (chunkFlushRaf) {
			cancelAnimationFrame(chunkFlushRaf);
			chunkFlushRaf = 0;
		}
		flushPendingChunks();
	}

	// Streaming chunk handler factory: finalizes the preceding reasoning block
	// on the first thought chunk, dedups by per-step seq, and queues the delta
	// for the per-frame flush (see flushPendingChunks).
	function chunkHandler(stepIdPrefix, msgType) {
		return (event) => {
			const data = event.payload;
			const tid = data.session_id;
			const sid = stepId(stepIdPrefix, tid, data.step_number, data.run_id);
			const delta = data.delta || '';
			const seq = data.seq;
			// The model-state chip reflects the ACTIVE conversation only:
			// a background session streaming in parallel must not flip the
			// active session's indicator to "streaming".
			if (activeSessionId === tid) {
				updateModelState('streaming');
			}
			if (seqLastSeen(sid, seq)) return;

			// Queue the chunk; the reasoning finalize + accumulation run in
			// order inside the flush, so per-event semantics are unchanged.
			pendingChunks.push({
				tid,
				sid,
				stepIdPrefix,
				delta,
				msgType,
				stepNumber: data.step_number,
				runId: data.run_id,
				time: new Date().toLocaleTimeString(),
				finalizeReasoning: stepIdPrefix === 'thought',
			});
			if (pendingChunks.length > PENDING_CHUNK_MAX) {
				pendingChunks.shift();
				pendingChunkDrops++;
				if (pendingChunkDrops === 1) {
					logger.warn(
						'+page',
						`chunk queue overflow (${PENDING_CHUNK_MAX}), evicting oldest chunks`,
					);
				}
			}
			if (!chunkFlushRaf) {
				chunkFlushRaf = requestAnimationFrame(flushPendingChunks);
			}
		};
	}

	// Populate the toolbar model switcher from a per-session cache so page
	// reloads (dev HMR reconnect, window re-show, single-instance re-entry)
	// don't re-request the same model list. Concurrent mounts share the
	// in-flight request, so the duplicate discover_models calls seen on
	// reload disappear without losing the fresh-on-first-load behavior.
	function ensureDefaultModelOptions(baseUrl) {
		if (defaultModelsCache.baseUrl === baseUrl && defaultModelsCache.list) {
			modelOptions = defaultModelsCache.list;
			return;
		}
		if (defaultModelsCache.inflight) {
			defaultModelsCache.inflight
				.then((list) => {
					if (!dead) modelOptions = list;
				})
				.catch(() => {
					if (!dead) modelOptions = [];
				});
			return;
		}
		defaultModelsCache.baseUrl = baseUrl;
		defaultModelsCache.inflight = invoke('discover_models', {
			baseUrl,
			apiKey: '',
			role: 'default_model',
		})
			.then((list) => {
				const next = list || [];
				defaultModelsCache.list = next;
				if (!dead) modelOptions = next;
				return next;
			})
			.catch((e) => {
				logger.warn('+page', 'discover_models error', e);
				if (!dead) modelOptions = [];
				throw e;
			})
			.finally(() => {
				defaultModelsCache.inflight = null;
			});
		// Swallow the rethrown rejection for the shared in-flight promise;
		// the branch above already surfaces the failure to the UI.
		defaultModelsCache.inflight.catch(() => {});
	}

	onMount(async () => {
		// Hydrate the fresh-start intent from localStorage BEFORE any data
		// load: the store is in-memory only, but the intent survives app
		// restarts via `haven.no_auto_restore`. Without this, `loadSessions`
		// auto-assign would re-select the old conversation on restart and the
		// persisted intent would be silently defeated. The reviewTarget
		// branch below (an explicit user choice) clears it again if needed.
		if (browser && localStorage.getItem(NEW_TASK_INTENT_KEY)) {
			newSessionIntentStore.set(true);
		}

		// Process review target first so loadSessions won't overwrite
		// activeSessionId with a stale paused session whose messages are gone.
		const reviewTarget = get(reviewTargetStore);
		if (reviewTarget && reviewTarget.sessionId) {
			// Opening a reviewed conversation abandons any pending fresh-start
			// intent (the user chose this conversation explicitly).
			newSessionIntentStore.set(false);
			if (browser) localStorage.removeItem(NEW_TASK_INTENT_KEY);
			activeSessionId = reviewTarget.sessionId;
			activeSessionIdStore.set(activeSessionId);
			// If this session was errored when reviewed, show the continue button.
			// reopen_session already set it to Paused, but we still want the user
			// to see the option to retry the failed step.
			if (reviewTarget.wasError) {
				sessionErrorId = reviewTarget.sessionId;
				activeSessionError = true;
			}
			// Defer clearing so it survives rapid remounts during init.
			setTimeout(() => reviewTargetStore.set(null), 0);
		}

		// Register listeners BEFORE any async data load so session/streaming
		// events arriving while the page initializes are never missed.
		const registrations = registerListeners(
			{
				'session:created': (event) => {
					const tid = event.payload?.session_id;
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
						// by submit.js when that submission's invoke resolves.
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
					const data = event.payload || {};
					const isActive = data.session_id && activeSessionId && data.session_id === activeSessionId;
					// A resume (pending) means the user's answer was received:
					// stop showing the awaiting indicator on ask cards. Note the
					// ask pause itself arrives as 'paused' right after the card is
					// created, so that status must NOT clear the indicator.
					if (isActive && data.status === 'pending') {
						clearAskAwaiting(data.session_id);
					}
					// A resumed session (pending/running) is no longer in the
					// errored state the continue banner describes: dismiss a
					// stale banner so it can't linger over a live generation
					// (e.g. when the retry started before the continue-session
					// invoke resolved, or a message resumed the session).
					if (
						data.session_id &&
						sessionErrorId === data.session_id &&
						(data.status === 'pending' || data.status === 'running')
					) {
						sessionErrorId = null;
						activeSessionError = false;
					}
					// A background session reaching a terminal state has no more
					// streaming events: evict its messages (switchToSession reloads
					// from the DB on demand) so completed conversations don't
					// accumulate in memory for the whole session.
					if (data.status === 'completed' || data.status === 'error') {
						evictTerminalSessionMemory(data.session_id);
					}
					loadSessions();
				},
				'session:completed': (event) => {
					const data = event.payload || {};
					if (data.session_id && activeSessionId && data.session_id === activeSessionId) {
						clearAskAwaiting(data.session_id);
					}
					evictTerminalSessionMemory(data.session_id);
					loadSessions();
				},
				'session:error': (event) => {
					const { session_id } = event.payload;
					if (session_id && session_id === activeSessionId) {
						sessionErrorId = session_id;
						activeSessionError = true;
						clearAskAwaiting(session_id);
						// The session died mid-tool-call: every streaming block
						// (tool placeholder, reasoning, thought) would stay
						// in its "expanded/streaming" state forever otherwise.
						// Finalize them all so the UI reflects the stop.
						updateSessionMessages(session_id, (m) =>
							m.map((x) => (x.streaming ? { ...x, streaming: false } : x))
						);
					}
					evictTerminalSessionMemory(session_id);
					loadSessions();
				},
				'session:title-updated': (event) => {
					const { session_id, title } = event.payload;
					const idx = sessions.findIndex((t) => t.id === session_id);
					if (idx >= 0) sessions[idx] = { ...sessions[idx], title };
				},
				'hotkey:rebind': (event) => {
					const data = event.payload || {};
					if (data.new_binding) {
						hotkeyBinding = data.new_binding;
					}
				},
				'agent:thought': (event) => {
					const data = event.payload;
					const tid = data.session_id;
					const thoughtId = stepId('thought', tid, data.step_number, data.run_id);
					const reasoningId = stepId('reasoning', tid, data.step_number, data.run_id);
					// The authoritative snap collapses the streamed segments:
					// apply any queued chunks first so no delta is left to
					// accumulate onto the collapsed message afterwards.
					flushChunksNow();
					pruneSeq(thoughtId);
					pruneSeq(reasoningId);
					// Deliberately no updateModelState here: the chunk handler
					// already left the chip in `streaming`, and forcing `ready`
					// on the thought snapshot causes a visible ready->tool flicker
					// when the step continues with tool calls. The next event
					// (agent:action -> tool, or pause/completion -> ready) owns
					// the transition.
					updateSessionMessages(tid, (m) =>
						applyThoughtSnap(m, {
							stepId: thoughtId,
							reasoningId,
							thought: data.thought,
							stepNumber: data.step_number,
							time: new Date().toLocaleTimeString(),
						}),
					);
				},
				'agent:thought_chunk': chunkHandler('thought', undefined),
				'agent:reasoning_chunk': chunkHandler('reasoning', 'reasoning'),
				'agent:web_search': (event) => {
					const data = event.payload || {};
					const tid = data.session_id;
					if (!tid || (activeSessionId && tid !== activeSessionId)) return;
					const wsId = toolId(tid, data.step_number, data.run_id, 'web_search');
					updateSessionMessages(tid, (m) => {
						const existing = m.find((x) => x.id === wsId);
						if (data.phase === 'completed') {
							if (!existing) return m;
							return m.map((x) =>
								x.id === wsId
									? { ...x, streaming: false, content: '已联网搜索' }
									: x,
							);
						}
						// in_progress / searching: keep the indicator alive.
						const content = data.phase === 'searching' ? '正在搜索…' : '正在联网搜索…';
						if (existing) {
							return m.map((x) => (x.id === wsId ? { ...x, content } : x));
						}
						return [
							...m,
							newToolMessage({
								id: wsId,
								stepNumber: data.step_number,
								toolName: 'web_search',
								time: new Date().toLocaleTimeString(),
								content,
								streaming: true,
							}),
						];
					});
				},
				'agent:supplement': (event) => {
					// The agent injected a user message (mid-turn steering or a
					// resumed-session supplement) into its context. Mark the matching
					// user bubble as received so the user knows their input was
					// picked up mid-turn rather than deferred.
					const data = event.payload || {};
					const tid = data.session_id;
					const ctx = (data.additional_context || '').trim();
					if (!tid || !ctx) return;
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
								next[i] = { ...x, received: true };
								marked = true;
								break;
							}
						}
						return marked ? next : m;
					});
				},
				'agent:action': (event) => {
					const data = event.payload;
					const tid = data.session_id;
					// A tool action finalizes the step's streaming blocks:
					// apply queued chunks first so the finalize is complete.
					flushChunksNow();
					updateModelState('tool');
					const toolMsgId = toolId(
						tid,
						data.step_number,
						data.run_id,
						data.tool_call_id || data.tool_name,
					);
					const reasoningId = stepId('reasoning', tid, data.step_number, data.run_id);
					const thoughtId = stepId('thought', tid, data.step_number, data.run_id);
					pruneSeq(reasoningId);
					pruneSeq(thoughtId);
					if (data.silent) {
						// Silent tool: no card is shown, but the preceding text
						// must still be finalized so it is inserted immediately.
						updateSessionMessages(tid, (m) =>
							finalizeStreamBlocks(m, reasoningId, thoughtId),
						);
						return;
					}
					updateSessionMessages(tid, (m) => {
						// Finalize any streaming reasoning and thought blocks —
						// a tool action means the text/reasoning phase is over.
						// Clearing `segmented` drops straggler chunks that flush
						// out of the batcher after this event.
						const fixed = finalizeStreamBlocks(m, reasoningId, thoughtId);
						const existing = fixed.find((x) => x.id === toolMsgId);
						if (existing) return fixed;
						return [
							...fixed,
							newToolMessage({
								id: toolMsgId,
								stepNumber: data.step_number,
								toolName: data.tool_name,
								time: new Date().toLocaleTimeString(),
								streaming: true,
							}),
						];
					});
				},
				'agent:observation': (event) => {
					const data = event.payload;
					if (data.silent) return;
					const tid = data.session_id;
					flushChunksNow();
					updateModelState('streaming');
					const toolMsgId = toolId(
						tid,
						data.step_number,
						data.run_id,
						data.tool_call_id || data.tool_name,
					);
					updateSessionMessages(tid, (m) => {
						const idx = m.findIndex((x) => x.id === toolMsgId);
						const msg = newToolMessage({
							id: toolMsgId,
							stepNumber: data.step_number,
							toolName: data.tool_name,
							content: data.observation,
							askOptions: data.ask_options || [],
						});
						if (idx >= 0) {
							// Preserve the fields set by the action handler (e.g. the
							// bubble's timestamp) — only overwrite the observation
							// content and related fields.
							const next = [...m];
							next[idx] = { ...next[idx], ...msg, streaming: false };
							return next;
						}
						return [...m, msg];
					});
				},
				'confirm:requested': (event) => {
					const data = event.payload;
					// Security confirmations are modal and resolve by step id, so
					// requests from background (non-active) sessions must still be
					// surfaced — dropping them would leave the tool call waiting
					// forever. The dialog shows which session the operation belongs
					// to so an approval is never misattributed.
					const tid = data.session_id || '';
					// Auto-reject a superseded dialog ONLY when the new request
					// belongs to the same session (a session firing a second
					// confirmation has moved on from the first — the backend
					// must not wait forever for a resolve it will never see).
					// A different session's request must NOT deny the pending one:
					// the user may have just approved it (the resolve races the
					// auto-reject), and its wait is already bounded by the
					// backend's fail-closed timeout.
					if (confirmDialog.stepId && confirmDialog.sessionId === tid) {
						invoke('resolve_confirmation', {
							stepId: confirmDialog.stepId,
							confirmed: false,
							trustSession: false,
						}).catch(() => {});
					}
					const session = sessions.find((t) => t.id === tid);
					confirmDialog = {
						stepId: data.step_id,
						toolName: data.tool_name,
						sessionId: tid,
						sessionTitle: session?.title || (tid || ''),
						riskLevel: data.risk_level || 'medium',
					};
				},
				// Token usage / cost stats — emitted after every LLM step.
				'agent:usage': (event) => {
					const d = event.payload || {};
					if (!d.session_id) return;
					updateSessionTokenStats(d.session_id, {
						promptTokens: d.prompt_tokens || 0,
						completionTokens: d.completion_tokens || 0,
						totalTokens: d.total_tokens || 0,
						cumulativePromptTokens: d.cumulative_prompt_tokens || 0,
						cumulativeCompletionTokens: d.cumulative_completion_tokens || 0,
						cumulativeTotalTokens: d.cumulative_total_tokens || 0,
						costUsd: d.cost_usd ?? null,
						cumulativeCostUsd: d.cumulative_cost_usd ?? null,
						contextWindow: d.context_window ?? null,
						model: d.model ?? null,
						// A real usage event supersedes any restored estimate.
						estimated: false,
						// A live event means the conversation is active again:
						// the widget switches back to the per-step context view.
						restored: false,
					});
				},
				// Context compaction notice — summarize a portion of the history.
				'agent:compaction': (event) => {
					const d = event.payload || {};
					const before = formatTokenCount(d.tokens_before || 0);
					const after = formatTokenCount(d.tokens_after || 0);
					addNotification(`上下文压缩：${before} → ${after} tokens`, 'info', 2500);
				},
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
				const dm = s?.llm?.default_model;
				if (dm?.model_name) {
					currentModelId = dm.model_name;
					currentModelName = dm.model_name;
				}
				currentEffort = dm?.reasoning_effort || '';
				currentWebSearch = dm?.web_search || 'off';
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
				if (dm?.base_url) {
					ensureDefaultModelOptions(dm.base_url);
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
		const restoreP = restoreLastConversation(reviewTarget);

		await Promise.all([sessionsP, restoreP, readyP]);

		// Conversation just opened (history review or auto-restore): scroll to
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
					const firstActive = sessions.find(
						(t) =>
							t.status === 'running' ||
							t.status === 'pending' ||
							t.status === 'paused',
					);
					if (firstActive) {
						activeSessionId = firstActive.id;
					}
				}
			}
			// Session lifecycle changes may have reaped background jobs (a session
			// ending cancels its jobs without terminal events): re-sync the
			// task board so the panel drops entries that no longer exist.
			// Same for reminders: fired ones are gone from the pending list.
			refreshTasks();
		})().catch((e) => {
			addNotification(`加载会话列表失败: ${e}`, 'error', 3000);
		});
		loadSessionsSettled = run;
		return run;
	}

	// Auto-restore the last conversation from a previous run so reopening
	// the app shows where you left off. Skipped when a review target is
	// pending, a session is already active, or the user explicitly started a
	// fresh conversation (新对话) and no new session has been created since.
	// Messages render as soon as `get_last_conversation` returns; the
	// follow-up `reopen_session` (which only lets follow-up messages continue
	// this session instead of being dropped as a terminal-session supplement) runs
	// afterwards without blocking the UI.
	async function restoreLastConversation(reviewTarget) {
		if (
			reviewTarget ||
			get(newSessionIntentStore) ||
			(browser && localStorage.getItem(NEW_TASK_INTENT_KEY))
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
		updateSessionMessages(last.session.id, () => buildReviewMessages(last));
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
	function clearAskAwaiting(sessionId) {
		updateSessionMessages(sessionId, (m) =>
			m.map((x) => (x.type === 'ask' ? { ...x, awaiting: false, resolved: null } : x)),
		);
		// A resume/end also invalidates any locally-chosen quick-reply answers
		// for the pending batch, so a later batch never inherits stale ones.
		resolvedAskIds.delete(sessionId);
	}

	async function submitMessage(text, images, files) {
		try {
			const result = await submitTranscript(text, { images, files });
			if (result && result.SessionCreated) {
				activeSessionId = result.SessionCreated;
				activeSessionIdStore.set(activeSessionId);
				// The submission itself created the session (submitTranscript
				// already cleared the intent store): nothing to do here.
			}
			loadSessions();
		} catch (e) {
			addNotification(`发送失败: ${e}`, 'error', 5000);
		}
	}

	// Entry point for the InputRouter component: it normalizes every input
	// format (typed text, pasted/picked images, attached files, voice) into a
	// single payload and forwards it here. The router already cleared its
	// draft, so the page just delivers the message and resumes auto-follow.
	function handleInputSubmit({ text, images, files }) {
		autoFollow = true;
		submitMessage(text, images, files);
	}

	// Quick-reply answers / ignores chosen for the CURRENT batch of pending
	// ask questions, per session. When the agent asks several questions in one
	// batch (multiple `ask` calls in a single step), the session must stay
	// paused until every question is resolved — answering only one would
	// resume the session and silently discard the others. Once all are answered
	// or ignored, a single composed reply is submitted. Typing a message in
	// the input box bypasses this and resumes immediately.
	let resolvedAskIds = new Map(); // sessionId -> Set<msgId>

	// Mark one pending ask card as resolved (answered via quick reply or
	// ignored) and submit the composed answers once the batch is complete.
	function resolveAsk(msgId, resolved) {
		if (!activeSessionId || !msgId) return;
		updateSessionMessages(activeSessionId, (m) =>
			m.map((x) =>
				x.id === msgId && x.type === 'ask' && x.awaiting
					? { ...x, awaiting: false, resolved }
					: x,
			),
		);
		const ids = resolvedAskIds.get(activeSessionId) || new Set();
		ids.add(msgId);
		resolvedAskIds.set(activeSessionId, ids);
		const remaining = (get(sessionMessagesStore)[activeSessionId] || []).filter(
			(x) => x.type === 'ask' && x.awaiting,
		);
		if (remaining.length === 0) {
			const submitted = resolvedAskIds.get(activeSessionId);
			resolvedAskIds.delete(activeSessionId);
			submitAskAnswers(activeSessionId, submitted);
		}
	}

	// Compose all answers chosen for the resolved batch into a single user
	// message and deliver it, which resumes the paused session. A single
	// question keeps the raw answer; multiple questions quote each one so the
	// model can map answers back to its questions. Ignored questions are
	// marked as 忽略.
	function submitAskAnswers(sessionId, resolvedIds) {
		if (!resolvedIds || resolvedIds.size === 0) return;
		const asks = (get(sessionMessagesStore)[sessionId] || []).filter(
			(x) => x.type === 'ask' && x.resolved && resolvedIds.has(x.id),
		);
		if (asks.length === 0) return;
		const single = asks.length === 1;
		const text = asks
			.map((x, i) => {
				const answer = x.resolved.ignored ? '忽略' : x.resolved.answer;
				return single ? answer : `关于「${x.content || `问题 ${i + 1}`}」：${answer}`;
			})
			.join('\n');
		autoFollow = true;
		submitMessage(text, []);
	}

	// The agent asked a question and offered quick-reply buttons. The answer
	// marks that question as resolved; the session resumes only when every
	// pending question in the batch is answered or ignored (see resolveAsk).
	function handleQuickReply(msgId, answer) {
		if (!activeSessionId || !answer) return;
		resolveAsk(msgId, { answer });
	}

	// The user chooses not to answer a pending question; counting as a
	// resolution so the batch can resume once all questions are handled.
	function handleIgnoreAsk(msgId) {
		if (!activeSessionId) return;
		resolveAsk(msgId, { ignored: true });
	}

	async function handleConfirm({ stepId, approved, trustSession }) {
		// Clear the dialog synchronously BEFORE awaiting the IPC round-trip.
		// If we only cleared it after `await invoke(...)`, a new
		// `confirm:requested` for the same session arriving during that window
		// would see the stale stepId and auto-reject (confirmed: false) the very
		// step the user just approved — the two resolves race and the denial can
		// win, so the user's Allow is reported as a rejection.
		const resolvedStep = stepId;
		confirmDialog = {
			stepId: null,
			toolName: '',
			sessionId: '',
			sessionTitle: '',
			riskLevel: 'medium',
		};
		if (!resolvedStep) return;
		try {
			await invoke('resolve_confirmation', {
				stepId: resolvedStep,
				confirmed: approved,
				trustSession: trustSession || false,
			});
		} catch (e) {
			addNotification(`确认失败: ${e}`, 'error', 3000);
		}
	}
</script>

<div class="chat-page">
	<ConfirmationDialog
		stepId={confirmDialog.stepId}
		toolName={confirmDialog.toolName}
		sessionId={confirmDialog.sessionId}
		sessionTitle={confirmDialog.sessionTitle}
		riskLevel={confirmDialog.riskLevel}
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
					<p>PC 语音助手 · 按 {hotkeyBinding} 开始录音，或直接输入指令</p>
				</div>
			{:else}
				<div class="message-list">
					{#each messages as msg, i (msg.id)}
						{@const isLast = i === messages.length - 1}
						<ChatBubble
							role={msg.role}
							content={msg.content}
							type={msg.type}
							voice={msg.voice}
							time={msg.time}
							streaming={msg.streaming && isLast}
							toolName={msg.toolName ?? ''}
							messageId={msg.id}
							stepNumber={msg.stepNumber}
							usage={msg.type === 'tool' ? stepUsage(msg.stepNumber) : null}
							attachments={msg.attachments}
							options={msg.options ?? []}
							awaiting={msg.awaiting ?? false}
							received={msg.received ?? false}
							resolved={msg.resolved ?? null}
							onContextMenu={handleContextMenu}
							onQuickReply={handleQuickReply}
							onIgnore={handleIgnoreAsk}
						/>
					{/each}
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
		{...inputLimits}
		onsubmit={handleInputSubmit}
		onstop={endSession}
	>
		{#snippet toolbarLeft()}
			<div class="session-switch">
				<button
					class="md-btn md-btn--outlined session-switch-btn"
						onclick={() => {
							if (showSessionMenu) {
								sessionMenuOpen = !sessionMenuOpen;
							} else {
								newSession();
							}
						}}
						title={showSessionMenu ? '切换并行会话或开始新会话' : '开始一个新会话'}
						type="button"
					>
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><line x1="12" y1="5" x2="12" y2="19" /><line
								x1="5"
								y1="12"
								x2="19"
								y2="12"
							/></svg
						>
						{#if showSessionMenu}
							<svg
								class="session-switch-caret"
								width="16"
								height="16"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg
							>
						{/if}
						{#if parallelSessions.length > 0}
							<span class="session-switch-badge">{parallelSessions.length}</span>
						{/if}
					</button>
					{#if sessionMenuOpen}
						<div class="session-menu">
							<div class="session-menu-title">正在执行的会话</div>
							{#each menuSessions as t}
								<button
									class="session-menu-item"
									class:selected={t.id === activeSessionId}
									onclick={() => switchToSession(t.id)}
									type="button"
								>
									<span class="session-menu-item-main">
										<span class="session-menu-item-title">{t.title}</span>
										<span class="session-menu-item-id">{t.id}</span>
									</span>
									<span
										class="session-menu-item-status"
										class:running={t.status === 'running'}
									>
										{t.status === 'running'
											? '运行中'
											: t.status === 'paused'
												? '已暂停'
												: '等待中'}
									</span>
								</button>
							{/each}
							<div class="session-menu-divider"></div>
							<button
								class="session-menu-item session-menu-new"
								onclick={() => newSession()}
								type="button"
							>
								<svg
									width="16"
									height="16"
									viewBox="0 0 24 24"
									fill="none"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
									stroke-linejoin="round"
									><line x1="12" y1="5" x2="12" y2="19" /><line
										x1="5"
										y1="12"
										x2="19"
										y2="12"
									/></svg
								>
								新建会话
							</button>
						</div>
					{/if}
				</div>
				{#if activeSessionId}
					<button
						class="md-btn md-btn--outlined end-session-btn"
						onclick={endSession}
						aria-label="结束会话"
						title="结束当前会话"
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
							><rect x="6" y="6" width="12" height="12" rx="2" /></svg
						>
					</button>
				{/if}
				<div
					class="token-stats"
					class:active={!!tokenStats}
					title={tokenStats ? buildTokenTooltip(tokenStats) : tokenStatsHint}
				>
					{#if tokenStats}
						<svg
							class="token-icon"
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<path d="M4 6h16M4 12h10M4 18h16" />
						</svg>
						<div class="token-text">
							<span class="token-context"
								>{formatTokenCount(
									tokenStats.restored
										? tokenStats.cumulativeTotalTokens || 0
										: tokenStats.promptTokens || 0,
								)}</span
							>
							<span class="token-unit">{tokenStats.restored ? 'tok' : 'ctx'}</span>
						</div>
						{#if contextBudget && !tokenStats.restored}
							<div
								class="token-budget"
								class:warn={contextBudget.ratio >= 0.75}
								class:danger={contextBudget.ratio >= 0.9}
								aria-label={`上下文使用 ${(contextBudget.ratio * 100).toFixed(0)}%`}
							>
								<div
									class="token-budget-fill"
									style="width: {(contextBudget.ratio * 100).toFixed(1)}%"
								></div>
							</div>
						{/if}
					{:else}
						<svg
							class="token-icon"
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<path d="M4 6h16M4 12h10M4 18h16" />
						</svg>
						<span class="token-text token-idle">—</span>
					{/if}
				</div>
		{/snippet}
		{#snippet toolbarRight()}
			<div class="model-switch">
					<button
						class="md-icon-button model-switch-btn"
						onclick={() => (modelMenuOpen = !modelMenuOpen)}
						title={`切换默认模型${currentModelName ? `：${currentModelName}` : ''}`}
						aria-label="切换默认模型"
						type="button"
					>
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="5" y="5" width="14" height="14" rx="2" /><rect
								x="9.5"
								y="9.5"
								width="5"
								height="5"
							/></svg
						>
					</button>
					{#if modelMenuOpen}
						<div class="model-menu">
							<div class="model-menu-title">切换默认模型</div>
							{#each modelOptions as m}
								<button
									class="model-item"
									class:selected={m.id === currentModelId}
									onclick={() => handleModelSelect(m)}
									type="button"
								>
									<span class="model-item-name">{m.name}</span>
									<span class="model-item-provider">{m.provider}</span>
								</button>
							{/each}
							<div class="model-menu-divider"></div>
							<div class="model-menu-title">思考强度</div>
							<div class="effort-row">
								{#each effortOptions as opt}
									<button
										class="effort-item"
										class:selected={currentEffort === opt.value}
										onclick={() => handleEffortSelect(opt.value)}
										type="button">{opt.label}</button
									>
								{/each}
							</div>
							<div class="model-menu-divider"></div>
							<div class="model-menu-title">联网搜索</div>
							<div class="effort-row">
								{#each webSearchOptions as opt}
									<button
										class="effort-item"
										class:selected={currentWebSearch === opt.value}
										onclick={() => handleWebSearchSelect(opt.value)}
										type="button">{opt.label}</button
									>
								{/each}
							</div>
						</div>
					{/if}
				</div>
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

	.session-switch {
		position: relative;
		flex-shrink: 0;
	}
	.session-switch-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.session-switch-caret {
		flex-shrink: 0;
	}
	.session-switch-badge {
		min-width: 18px;
		height: 18px;
		padding: 0 5px;
		border-radius: 999px;
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-size: 11px;
		font-weight: 700;
		line-height: 18px;
		text-align: center;
		font-variant-numeric: tabular-nums;
	}
	.end-session-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		color: var(--md-sys-color-error);
		border-color: var(--md-sys-color-error);
	}
	.end-session-btn:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.session-menu {
		position: absolute;
		left: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-width: 320px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.session-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.session-menu-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.session-menu-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.session-menu-item.selected .session-menu-item-title {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.session-menu-item-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		flex: 1 1 auto;
	}
	.session-menu-item-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.session-menu-item-id {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		font-family: var(--md-sys-typescale-body-small-font-family, inherit);
	}
	.session-menu-item-status {
		flex-shrink: 0;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.session-menu-item-status.running {
		color: var(--md-sys-color-primary);
	}
	.session-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.session-menu-new {
		justify-content: flex-start;
		gap: var(--md-sys-space-sm);
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.token-stats {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 84px;
		height: 40px;
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-corner-medium, 8px);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container, transparent);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		line-height: 1;
		flex-shrink: 0;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.token-stats.active {
		border-color: var(--md-sys-color-primary);
	}
	.token-icon {
		opacity: 0.75;
		flex-shrink: 0;
	}
	.token-text {
		display: inline-flex;
		gap: 4px;
		align-items: baseline;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.token-context {
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.token-unit {
		opacity: 0.6;
		font-size: 10px;
	}
	.token-idle {
		opacity: 0.5;
	}
	.token-budget {
		position: relative;
		width: 36px;
		height: 4px;
		border-radius: 999px;
		background: var(--md-sys-color-surface-variant, rgba(0, 0, 0, 0.06));
		overflow: hidden;
		flex-shrink: 0;
	}
	.token-budget-fill {
		position: absolute;
		inset: 0 auto 0 0;
		background: var(--md-sys-color-primary);
		transition:
			width var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard),
			background var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard);
	}
	.token-budget.warn .token-budget-fill {
		background: #c97a00;
	}
	.token-budget.danger .token-budget-fill {
		background: var(--md-sys-color-error, #b3261e);
	}
	.model-switch {
		position: relative;
		flex-shrink: 0;
	}
	.model-menu {
		position: absolute;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.model-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.model-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.model-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.model-item.selected .model-item-name {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.model-item-provider {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.model-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.effort-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
	}
	.effort-item {
		flex: 1;
		height: 32px;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-weight: 600;
		font-family: inherit;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.effort-item:hover {
		border-color: var(--md-sys-color-primary);
	}
	.effort-item.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
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
</style>
