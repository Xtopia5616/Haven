import { writable } from 'svelte/store';
import { invoke } from './tauri.js';
import logger from '$lib/logger.js';

export const taskStore = writable([]);

/**
 * Activity registry (background jobs + pending reminders): `{ [id]: Activity }`
 * where each entry mirrors a row from the backend's `list_activities`:
 *   { id, kind: 'job'|'reminder', task_id?, status?, started_at?,
 *     finished_at?, due_at?, preview?, output?, error?, title?, body?, ... }
 * Job rows keep `job_id` and status fields; reminder rows keep `id` and
 * due_at. `id` is normalized to the entry key for both.
 * Kept in sync by the `activity:created` / `activity:updated` /
 * `activity:output` / `activity:finished` events (registered in
 * +layout.svelte, hydrated via `refreshActivities`).
 */
export const activityStore = writable({});

/** Cap terminal entries so a long session cannot grow the store unbounded. */
const ACTIVITY_STORE_MAX = 64;

function activityKey(payload) {
	return payload?.id || payload?.job_id || null;
}

export function upsertActivity(payload) {
	const key = activityKey(payload);
	if (!key) return;
	activityStore.update((m) => {
		const prev = m[key] || {};
		const next = { ...prev, ...payload, id: key, kind: payload.kind || prev.kind || (payload.job_id ? 'job' : 'reminder') };
		// Terminal entries keep their full payload (output/error) so the
		// panel can show the result; only the store size is bounded below.
		const entries = { ...m, [key]: next };
		const ids = Object.keys(entries);
		if (ids.length > ACTIVITY_STORE_MAX) {
			const excess = ids.length - ACTIVITY_STORE_MAX;
			for (const id of ids.slice(0, excess)) delete entries[id];
		}
		return entries;
	});
}

/** Drop an activity (fired or cancelled reminder, job removed server-side). */
export function removeActivity(id) {
	if (!id) return;
	activityStore.update((m) => {
		if (!(id in m)) return m;
		const next = { ...m };
		delete next[id];
		return next;
	});
}

export async function refreshActivities() {
	try {
		const rows = await invoke('list_activities');
		if (!Array.isArray(rows)) return;
		// Replace the registry: entries missing from the board were removed
		// server-side (a task ending cancels its jobs without terminal
		// events, fired reminders leave the pending list), so they must not
		// linger as stale rows.
		activityStore.update((m) => {
			const next = {};
			for (const row of rows) {
				const key = activityKey(row);
				if (key) next[key] = { ...(m[key] || {}), ...row, id: key };
			}
			return next;
		});
	} catch (e) {
		logger.warn('stores', 'refreshActivities failed', e);
	}
}

export async function cancelActivity(id, kind = 'job') {
	return invoke('cancel_activity', { activityId: id, kind });
}

/**
 * Fired-reminder history (and terminal job history) from the persisted
 * activity table, newest first. Returns the raw rows for the panel's history
 * tab; the caller owns the list (no store backing — it is fetched on demand).
 * @param {string} [kind]
 * @param {number} [limit]
 * @returns {Promise<Array>}
 */
export async function refreshActivityHistory(kind = 'reminder', limit = 50) {
	try {
		const rows = await invoke('list_activity_history', { kind, limit });
		return Array.isArray(rows) ? rows : [];
	} catch (e) {
		logger.warn('stores', 'refreshActivityHistory failed', e);
		return [];
	}
}

/** Delete a persisted activity row (history cleanup) by id. */
export async function deleteActivity(id) {
	return invoke('delete_activity', { activityId: id });
}

export const notificationStore = writable([]);

let notificationSeq = 0;

export function addNotification(msg, type = 'info', duration = 3000) {
	if (type === 'error') {
		logger.error('notification', msg);
	}
	let id = null;
	notificationStore.update((n) => {
		if (n.some((x) => x.msg === msg && x.type === type)) {
			return n;
		}
		// L10: monotonic sequence (plus randomness) so two notifications
		// created in the same millisecond cannot collide.
		id = `${Date.now()}-${notificationSeq++}-${Math.random().toString(36).slice(2, 6)}`;
		return [...n, { id, msg, type }];
	});
	if (id !== null) {
		setTimeout(() => {
			notificationStore.update((n) => n.filter((x) => x.id !== id));
		}, duration);
	}
}

// Per-task message storage: { [taskId: string]: Message[] }
// Special key '_draft' holds messages that haven't been assigned to a task yet
// (e.g. transcribed text before the task is created).
export const taskMessagesStore = writable({});

export const DRAFT_KEY = '_draft';

export function setTaskMessages(taskId, messages) {
	taskMessagesStore.update((m) => ({ ...m, [taskId]: messages }));
}

export function addTaskMessage(taskId, msg) {
	taskMessagesStore.update((m) => {
		const list = m[taskId] || [];
		return { ...m, [taskId]: [...list, msg] };
	});
}

export function updateTaskMessages(taskId, fn) {
	taskMessagesStore.update((m) => {
		const list = m[taskId] || [];
		const nextList = fn(list);
		// Skip the write when the updater returned the same array reference
		// (a no-op): Svelte stores notify every subscriber on update, and the
		// streaming path calls this once per chunk.
		if (nextList === list) return m;
		return { ...m, [taskId]: nextList };
	});
}

// Track per-step streaming sequence numbers to detect and reject duplicates
// from Tauri event replay after page navigation.
const seqMap = new Map();
export function seqLastSeen(stepId, seq) {
	if (seq == null) return false;
	const last = seqMap.get(stepId) ?? -1;
	if (seq <= last) return true;
	seqMap.set(stepId, seq);
	return false;
}

/** Remove seq tracking for a completed step to keep the map bounded. */
export function pruneSeq(stepId) {
	seqMap.delete(stepId);
}

export function clearSeqMap(taskId) {
	for (const key of seqMap.keys()) {
		if (key.includes(taskId)) seqMap.delete(key);
	}
}

export function clearTaskMessages(taskId) {
	if (!taskId) return;
	clearSeqMap(taskId);
	taskMessagesStore.update((m) => {
		const next = { ...m };
		delete next[taskId];
		return next;
	});
}

// Internal: find the index to cut at for truncate/branch. Skips user
// messages (they carry no stepNumber in the live view; the review
// builder assigns them the FOLLOWING assistant's stepNumber — cutting
// ON a user message would drop user input from the view even though
// the backend kept it).
function cutIndexForStep(list, targetStep) {
	return list.findIndex(
		(x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
	);
}

/**
 * Remove all messages at or after the given step number for a task.
 * Used by rollback: the ReAct loop will re-execute from `targetStep`, so
 * any messages belonging to that step or later are stale and must be
 * dropped from the UI. User messages (no stepNumber) that appear after the
 * first removed message are also dropped since they belong to the discarded
 * timeline.
 *
 * The cut lands on the first NON-user message at/after the target step.
 * User messages carry no stepNumber in the live view, but the review
 * builder assigns them the stepNumber of the FOLLOWING assistant message —
 * cutting ON a user message would drop the user's input from the view even
 * though the backend kept it in the session (rollback only discards
 * messages persisted after the branch point).
 */
export function truncateTaskMessages(taskId, targetStep) {
	if (!taskId) return;
	taskMessagesStore.update((m) => {
		const list = m[taskId];
		if (!list || list.length === 0) return m;
		const cutIdx = cutIndexForStep(list, targetStep);
		if (cutIdx === -1) return m;
		const next = { ...m };
		next[taskId] = list.slice(0, cutIdx);
		return next;
	});
	// Clear all seq tracking for this task. Remaining messages (before the
	// rollback point) are already finalized, so their seq entries are stale
	// anyway. This avoids fragile key-string parsing for step numbers.
	clearSeqMap(taskId);
}

// Move all messages from `fromKey` to `toKey` in a single store update.
// No-op when `fromKey` is missing, empty, or equal to `toKey`.
function _moveMessages(m, fromKey, toKey) {
	if (!fromKey || !toKey || fromKey === toKey) return m;
	const list = m[fromKey];
	if (!list || list.length === 0) return m;
	const next = { ...m };
	next[fromKey] = [];
	// Migrated messages (adoptDraftMessages / moveTaskMessages) are the user
	// input that CREATED the target task, so they logically precede any agent
	// content already in `toKey`. The backend can stream the first
	// "Thinking…" reasoning block before the task:created handler migrates the
	// optimistic user bubble; appending (old behavior) then renders the user's
	// opening message AFTER the reasoning. Prepend instead so the user input
	// always leads the conversation.
	//
	// The task was created because the agent accepted this input, so the
	// migrated user message(s) are already "received": mark them so the ✓
	// shows on the very first bubble too (the `agent:supplement` event only
	// covers mid-turn steering, never the opening message).
	next[toKey] = [
		...list.map((x) => (x.role === 'user' ? { ...x, received: true } : x)),
		...(next[toKey] || []),
	];
	return next;
}

// Move draft messages to a real task (called when task:created fires).
export function adoptDraftMessages(taskId) {
	taskMessagesStore.update((m) => _moveMessages(m, DRAFT_KEY, taskId));
}

/**
 * Move messages between task keys. Used when the backend reports
 * `TaskCreated` for a voice/typed submission whose messages were appended
 * under a different key — either `_draft` (no task was open) or a stale task
 * id the UI auto-restored while STT was running. Without the move, the user's
 * message would stay hidden in the old key while the new task only shows the
 * agent's reply.
 */
export function moveTaskMessages(fromTaskId, toTaskId) {
	taskMessagesStore.update((m) => _moveMessages(m, fromTaskId, toTaskId));
}

// Review target for navigating from history to chat with a task context.
// Set by history page before navigating to /, consumed by +page.svelte on mount.
export const reviewTargetStore = writable(null);

// Active task ID that persists across SvelteKit page navigations so the
// send handler and voice recording can supplement the same task.
export const activeTaskIdStore = writable(null);

// localStorage key recording an explicit "start a fresh conversation" intent
// that survives app restarts (set by the new-task button, cleared when the
// intent is fulfilled or abandoned). Mirrored into `newTaskIntentStore` for
// the live session.
export const NEW_TASK_INTENT_KEY = 'haven.no_auto_restore';

/**
 * Sticky intent flag: the user explicitly asked for a NEW task (new-task
 * button). While set, NO event-driven path may auto-assign an existing task
 * to `activeTaskId` (loadTasks auto-assign, task:created, auto-restore) —
 * otherwise the next message would append to the old conversation. Cleared
 * only when the intent is fulfilled (a new task was created by the user's
 * own submission) or abandoned (explicit switch to another task).
 */
export const newTaskIntentStore = writable(false);

/**
 * Per-task token usage + cost reported by the agent. Keyed by task id.
 * Updated on every `agent:usage` event so the chat toolbar can show
 * running totals and remaining context budget.
 *
 * Shape: { [taskId]: {
 *   promptTokens, completionTokens, totalTokens,
 *   cumulativePromptTokens, cumulativeCompletionTokens, cumulativeTotalTokens,
 *   costUsd, cumulativeCostUsd, contextWindow, model,
 *   lastUpdated: number,
 * }}
 */
export const taskTokenStatsStore = writable({});

/**
 * Update (or insert) the token-stats entry for a task. Replaces the whole
 * task entry so stale fields don't accumulate across event variants.
 * @param {string} taskId
 * @param {object} stats
 */
export function updateTaskTokenStats(taskId, stats) {
	if (!taskId) return;
	taskTokenStatsStore.update((m) => ({
		...m,
		[taskId]: { ...(m[taskId] || {}), ...stats, lastUpdated: Date.now() },
	}));
}

/** Clear token stats for a finished/reset task. */
export function clearTaskTokenStats(taskId) {
	if (!taskId) return;
	taskTokenStatsStore.update((m) => {
		if (!(taskId in m)) return m;
		const next = { ...m };
		delete next[taskId];
		return next;
	});
}

/**
 * Restore token stats for a task from persisted backend usage counters
 * (returned by get_task_for_review / get_last_conversation). Cumulative
 * totals are the persisted running totals; per-step and budget fields stay
 * empty until the next `agent:usage` event. When the task predates usage
 * persistence, `estimated` marks the restored totals as a rough estimate
 * derived from the persisted conversation text (no cost), which the widget
 * renders with an "约" prefix.
 * @param {string} taskId
 * @param {object} usage - { prompt_tokens, completion_tokens, total_tokens, cost_usd, has_cost }
 * @param {boolean} [estimated]
 */
export function restoreTaskTokenStats(taskId, usage, estimated = false) {
	if (!taskId || !usage) return;
	const hasCost = !!usage.has_cost && usage.cost_usd != null;
	updateTaskTokenStats(taskId, {
		promptTokens: 0,
		completionTokens: 0,
		totalTokens: 0,
		cumulativePromptTokens: usage.prompt_tokens || 0,
		cumulativeCompletionTokens: usage.completion_tokens || 0,
		cumulativeTotalTokens: usage.total_tokens || 0,
		costUsd: null,
		cumulativeCostUsd: hasCost ? usage.cost_usd : null,
		contextWindow: null,
		model: null,
		estimated: !!estimated,
	});
}

/**
 * Format a token count for compact display. Examples:
 *   123         -> "123"
 *   12_345      -> "12.3K"
 *   1_234_567   -> "1.23M"
 * @param {number} n
 * @returns {string}
 */
export function formatTokenCount(n) {
	const v = Number(n) || 0;
	if (v < 1_000) return String(v);
	if (v < 10_000) {
		const s = (v / 1_000).toFixed(2).replace(/\.?0+$/, '');
		return s + 'K';
	}
	if (v < 1_000_000) {
		const s = (v / 1_000).toFixed(1).replace(/\.0$/, '');
		return s + 'K';
	}
	return (v / 1_000_000).toFixed(2).replace(/\.?0+$/, '') + 'M';
}

/**
 * Format a USD cost. Examples:
 *   0          -> "$0.00"
 *   0.00123    -> "$0.0012"
 *   0.1234     -> "$0.123"
 *   1.5        -> "$1.50"
 * @param {number | null | undefined} v
 */
export function formatCostUsd(v) {
	if (v == null || !Number.isFinite(v)) return null;
	if (v === 0) return '$0.00';
	if (v < 0.01) return `$${v.toFixed(4)}`;
	if (v < 1) return `$${v.toFixed(3)}`;
	return `$${v.toFixed(2)}`;
}

/**
 * Build a `data:` URL from a message attachment ({ media_type, data } where
 * data is base64 without the prefix). Shared by the input area previews and
 * ChatBubble rendering.
 * @param {{ media_type: string, data: string }} att
 */
export function imageDataUrl(att) {
	return `data:${att.media_type};base64,${att.data}`;
}

/**
 * Format a message timestamp for bubble display. Messages from today show
 * the wall-clock time (matching live streaming bubbles); older messages
 * show the full `yyyy/mm/dd hh:mm:ss` so history stays navigable. Both the
 * live path (Date) and the review path (RFC3339 `created_at` string) share
 * this helper so a merged list never mixes formats.
 * @param {Date|string|number} input
 * @returns {string}
 */
export function formatMessageTime(input) {
	const d = input instanceof Date ? input : new Date(input);
	const now = new Date();
	const sameDay =
		d.getFullYear() === now.getFullYear() &&
		d.getMonth() === now.getMonth() &&
		d.getDate() === now.getDate();
	if (sameDay) return d.toLocaleTimeString();
	const y = d.getFullYear();
	const m = String(d.getMonth() + 1).padStart(2, '0');
	const day = String(d.getDate()).padStart(2, '0');
	const h = String(d.getHours()).padStart(2, '0');
	const min = String(d.getMinutes()).padStart(2, '0');
	const s = String(d.getSeconds()).padStart(2, '0');
	return `${y}/${m}/${day} ${h}:${min}:${s}`;
}

/**
 * @param {{ role: string, content: string, type?: string|null, voice?: boolean, time?: string, attachments?: Array<{media_type: string, data: string}>, idPrefix?: string }} opts
 */
export function newMessage({ role, content, type = null, voice = false, time, attachments = [], idPrefix = '' }) {
	return {
		id: `${Date.now()}${idPrefix ? `-${idPrefix}` : ''}-${Math.random().toString(36).slice(2, 6)}`,
		role,
		content,
		type,
		voice,
		time: time || formatMessageTime(new Date()),
		attachments,
	};
}

// Shared recording UI state consumed by the layout overlay.
export const recordingOverlay = writable({
	visible: false,
	isRecording: false,
	processing: false,
	sessionId: null,
	startedAt: null,
	reason: null,
	vadState: 'silent',
});

// Model state for the status chip in the titlebar.
// Driven by +page.svelte's agent:* event handlers; consumed by +layout.svelte.
export const modelStateStore = writable('ready');

let modelStateTimer = null;
export function updateModelState(state, { idleTimeoutMs } = /** @type {{ idleTimeoutMs?: number }} */ ({})) {
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
	modelStateStore.set(state);
	if (state === 'waiting') {
		modelStateTimer = setTimeout(() => {
			modelStateTimer = null;
			modelStateStore.update((s) => (s === 'waiting' ? 'ready' : s));
		}, idleTimeoutMs ?? 5000);
	} else if (state === 'streaming') {
		modelStateTimer = setTimeout(() => {
			modelStateTimer = null;
			modelStateStore.update((s) => (s === 'streaming' ? 'ready' : s));
		}, idleTimeoutMs ?? 2000);
	}
}

export function clearModelStateTimer() {
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
}
