import { get, writable } from 'svelte/store';
import { invoke } from './tauri.ts';
import logger from '$lib/logger.ts';
import { mapActionPayload, type ActionKind, type ActionPayload } from './contracts/action.ts';

export const sessionStore = writable<any[]>([]);

/**
 * Live foreground tool-output previews keyed by `step-*` id.
 * Populated by `agent:tool_output`; cleared on `agent:observation`.
 * Kept out of the message list so ticks do not rewrite the transcript store.
 */
export const toolOutputPreviewStore = writable<Record<string, string>>({});

export function setToolOutputPreview(stepId: string, output: string) {
	if (!stepId) return;
	toolOutputPreviewStore.update((m) => {
		if (m[stepId] === output) return m;
		return { ...m, [stepId]: output };
	});
}

export function clearToolOutputPreview(stepId: string) {
	if (!stepId) return;
	toolOutputPreviewStore.update((m) => {
		if (!(stepId in m)) return m;
		const next = { ...m };
		delete next[stepId];
		return next;
	});
}

/**
 * Action registry (background actions + pending scheduled actions):
 * `{ [id]: Action }` where each entry mirrors a row from the backend's
 * `list_actions`:
 *   { id, kind: 'background'|'scheduled', sessionId?, status?, startedAt?,
 *     finishedAt?, dueAt?, preview?, output?, error?, title?, body?, ... }
 * The action contract supplies a uniform id for both task kinds.
 * Kept in sync by the `action:created` / `action:updated` / `action:output` /
 * `action:finished` events (registered in +layout.svelte, hydrated via
 * `refreshActions`).
 */
type ActionEntry = ActionPayload;
export const actionStore = writable<Record<string, ActionEntry>>({});

/** Cap terminal entries so a long session cannot grow the store unbounded. */
const ACTION_STORE_MAX = 64;

/** Live board rows that must never be evicted to make room for history. */
function isLiveActionRow(entry: ActionEntry) {
	if (entry.kind === 'scheduled') return true;
	return entry.status === 'running';
}

function trimActionStore(entries: Record<string, ActionEntry>) {
	const ids = Object.keys(entries);
	if (ids.length <= ACTION_STORE_MAX) return entries;
	const excess = ids.length - ACTION_STORE_MAX;
	// Prefer dropping terminal background rows; never drop running background
	// or pending scheduled rows (those back the titlebar badge/chip).
	const victims = ids.filter((id) => !isLiveActionRow(entries[id]));
	let removed = 0;
	for (const id of victims) {
		if (removed >= excess) break;
		delete entries[id];
		removed++;
	}
	return entries;
}

export function upsertAction(payload: ActionPayload) {
	const key = payload.id;
	if (!key) return;
	actionStore.update((m) => {
		const prev = m[key];
		const next: ActionEntry = {
			...prev,
			...payload,
		};
		// Terminal entries keep their full payload (output/error) so the
		// panel can show the result; only the store size is bounded below.
		return trimActionStore({ ...m, [key]: next });
	});
}

/** Drop a action (fired or cancelled scheduled action, action removed server-side). */
export function removeAction(id: string) {
	if (!id) return;
	actionStore.update((m) => {
		if (!(id in m)) return m;
		const next = { ...m };
		delete next[id];
		return next;
	});
}

export async function refreshActions() {
	try {
		const rows = await invoke('list_actions');
		if (!Array.isArray(rows)) return;
		// Replace the registry: entries missing from the board were removed
		// server-side (a session ending cancels its actions without terminal
		// events, fired scheduled actions leave the pending list), so they must
		// not linger as stale rows.
		actionStore.update((m) => {
			const next: Record<string, ActionEntry> = {};
			for (const wireRow of rows) {
				const row = mapActionPayload(wireRow as never);
				const key = row.id;
				if (!key) continue;
				const merged: ActionEntry = {
					...(m[key] || {}),
					...row,
					id: key,
				};
				// Terminal background history is loaded on demand via
				// `refreshActionHistory`; keeping it here bloated the store and
				// let ACTION_STORE_MAX eviction delete live running rows.
				if (merged.kind === 'background' && merged.status && merged.status !== 'running') {
					continue;
				}
				next[key] = merged;
			}
			return trimActionStore(next);
		});
	} catch (e) {
		logger.warn('stores', 'refreshActions failed', e);
	}
}

export async function cancelAction(id: string, kind: ActionKind = 'background') {
	return invoke('cancel_action', { actionId: id, kind });
}

/**
 * Fired-scheduled-action history (and terminal background-action history) from
 * the persisted action table, newest first. Returns the raw rows for the
 * panel's history tab; the caller owns the list (no store backing — it is
 * fetched on demand).
 * @param {string} [kind]
 * @param {number} [limit]
 * @returns {Promise<Array>}
 */
export async function refreshActionHistory(kind: ActionKind | null = 'scheduled', limit = 50) {
	try {
		const rows = await invoke('list_action_history', { kind, limit });
		return Array.isArray(rows) ? rows.map((row) => mapActionPayload(row as never)) : [];
	} catch (e) {
		logger.warn('stores', 'refreshActionHistory failed', e);
		return [];
	}
}

/** Delete a persisted action row (history cleanup) by id. */
export function deleteAction(id: string) {
	return invoke('delete_action', { actionId: id });
}

/**
 * Persist a terminal background-action payload onto any tool cards still
 * bound via `actionId`, then clear that bind so the card cannot fall back to
 * the original "running" observation ack.
 */
export function finalizeBackgroundActionMessages(payload: ActionPayload) {
	if (payload.kind !== 'background') return;
	const actionId = payload.id;
	if (!actionId) return;
	const status = payload.status ?? 'completed';
	const rawOut = payload.output ?? payload.error ?? '';
	const finalContent =
		typeof rawOut === 'string' && rawOut.trim().startsWith('{')
			? rawOut
			: JSON.stringify({
					output: rawOut,
					background: true,
					action_id: actionId,
					status,
					...(payload.exitCode != null ? { exit_code: payload.exitCode } : {}),
					...(payload.error && !payload.output ? { error: payload.error } : {}),
				});
	const all = get(sessionMessagesStore) || {};
	for (const tid of Object.keys(all)) {
		updateSessionMessages(tid, (m) => {
			let changed = false;
			const next = m.map((msg) => {
				if (msg.actionId !== actionId) return msg;
				changed = true;
				return {
					...msg,
					content: finalContent,
					actionId: null,
					streaming: false,
				};
			});
			return changed ? next : m;
		});
	}
}

export const notificationStore = writable<Array<{ id: string; msg: string; type: string }>>([]);

let notificationSeq = 0;

export function addNotification(msg: string, type = 'info', duration = 3000) {
	if (type === 'error') {
		logger.error('notification', msg);
	}
	let id: string | null = null;
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

// Per-session message storage: { [sessionId: string]: Message[] }
// Special key '_draft' holds messages that haven't been assigned to a session yet
// (e.g. transcribed text before the session is created).
export const sessionMessagesStore = writable<Record<string, any[]>>({});

export const DRAFT_KEY = '_draft';

export function setSessionMessages(sessionId: string, messages: unknown[]) {
	sessionMessagesStore.update((m) => ({ ...m, [sessionId]: messages }));
}

export function addSessionMessage(sessionId: string, msg: unknown) {
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] || [];
		return { ...m, [sessionId]: [...list, msg] };
	});
}

export function updateSessionMessages(sessionId: string, fn: (list: any[]) => any[]) {
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] || [];
		const nextList = fn(list);
		// Skip the write when the updater returned the same array reference
		// (a no-op): Svelte stores notify every subscriber on update, and the
		// streaming path calls this once per chunk.
		if (nextList === list) return m;
		return { ...m, [sessionId]: nextList };
	});
}

// Track per-step streaming sequence numbers to detect and reject duplicates
// from Tauri event replay after page navigation. Each entry also records the
// session the message belongs to so `clearSeqMap` can prune a whole session's
// bookkeeping (streaming message ids carry no session prefix, so a plain
// `includes` check could never match).
const seqMap = new Map();
const seqSessionOf = new Map();
/** @param {string} stepId @param {number|null|undefined} seq @param {string|null} [sessionId] */
export function seqLastSeen(stepId: string, seq: number | null | undefined, sessionId: string | null = null) {
	if (seq == null) return false;
	if (sessionId) seqSessionOf.set(stepId, sessionId);
	const last = seqMap.get(stepId) ?? -1;
	if (seq <= last) return true;
	seqMap.set(stepId, seq);
	return false;
}

/** Remove seq tracking for a completed step to keep the map bounded. */
export function pruneSeq(stepId: string) {
	seqMap.delete(stepId);
	seqSessionOf.delete(stepId);
}

/** Drop seq tracking for every streamed message of one session. */
export function clearSeqMap(sessionId: string) {
	if (!sessionId) return;
	for (const [stepId, sid] of seqSessionOf) {
		if (sid === sessionId) {
			seqMap.delete(stepId);
			seqSessionOf.delete(stepId);
		}
	}
}

export function clearSessionMessages(sessionId: string) {
	if (!sessionId) return;
	clearSeqMap(sessionId);
	sessionMessagesStore.update((m) => {
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Clear every per-session message list while keeping the un-sent `_draft`
 * (transcribed/typed input that was never submitted). Used after clearing
 * history: the draft belongs to no session and must survive the wipe.
 */
export function clearAllSessionMessages() {
	sessionMessagesStore.update((m) => {
		const next: Record<string, any[]> = {};
		if (Array.isArray(m[DRAFT_KEY]) && m[DRAFT_KEY].length > 0) {
			next[DRAFT_KEY] = m[DRAFT_KEY];
		}
		return next;
	});
}

// Internal: find the index to cut at for truncate/branch. Skips user
// messages (they carry no stepNumber in the live view; the resume
// builder assigns them the FOLLOWING assistant's stepNumber — cutting
// ON a user message would drop user input from the view even though
// the backend kept it).
function cutIndexForStep(list: any[], targetStep: number) {
	return list.findIndex(
		(x) => x.stepNumber != null && x.stepNumber >= targetStep && x.role !== 'user',
	);
}

/**
 * Remove all messages at or after the given step number for a session.
 * Used by rollback: the ReAct loop will re-execute from `targetStep`, so
 * any messages belonging to that step or later are stale and must be
 * dropped from the UI. User messages (no stepNumber) that appear after the
 * first removed message are also dropped since they belong to the discarded
 * timeline.
 *
 * The cut lands on the first NON-user message at/after the target step.
 * User messages carry no stepNumber in the live view, but the resume
 * builder assigns them the stepNumber of the FOLLOWING assistant message —
 * cutting ON a user message would drop the user's input from the view even
 * though the backend kept it in the session (rollback only discards
 * messages persisted after the branch point).
 */
export function truncateSessionMessages(sessionId: string, targetStep: number) {
	if (!sessionId) return;
	sessionMessagesStore.update((m) => {
		const list = m[sessionId] as unknown[] | undefined;
		if (!list || list.length === 0) return m;
		const cutIdx = cutIndexForStep(list, targetStep);
		if (cutIdx === -1) return m;
		const next = { ...m };
		next[sessionId] = list.slice(0, cutIdx);
		return next;
	});
	// Clear all seq tracking for this session. Remaining messages (before the
	// rollback point) are already finalized, so their seq entries are stale
	// anyway. This avoids fragile key-string parsing for step numbers.
	clearSeqMap(sessionId);
}

// Move all messages from `fromKey` to `toKey` in a single store update.
// No-op when `fromKey` is missing, empty, or equal to `toKey`.
function _moveMessages(m: Record<string, any[]>, fromKey: string, toKey: string) {
	if (!fromKey || !toKey || fromKey === toKey) return m;
	const list = m[fromKey];
	if (!list || list.length === 0) return m;
	const next = { ...m };
	next[fromKey] = [];
	// Migrated messages (adoptDraftMessages / moveSessionMessages) are the user
	// input that CREATED the target session, so they logically precede any agent
	// content already in `toKey`. The backend can stream the first
	// "Thinking…" reasoning block before the session:created handler migrates the
	// optimistic user bubble; appending (old behavior) then renders the user's
	// opening message AFTER the reasoning. Prepend instead so the user input
	// always leads the conversation.
	//
	// The session was created because the agent accepted this input, so the
	// migrated user message(s) are already "received": mark them so the ✓
	// shows on the very first bubble too (the `agent:supplement` event only
	// covers mid-turn steering, never the opening message).
	// Opening migrate: mark received and drop any sticky `steering` that a
	// parallel busy session's global modelState may have stamped on draft.
	next[toKey] = [
		...list.map((x) =>
			x.role === 'user' ? { ...x, received: true, steering: false } : x,
		),
		...(next[toKey] || []),
	];
	return next;
}

// Move draft messages to a real session (called when session:created fires).
export function adoptDraftMessages(sessionId: string) {
	sessionMessagesStore.update((m) => _moveMessages(m, DRAFT_KEY, sessionId));
}

/**
 * Move messages between session keys. Used when the backend reports
 * `SessionCreated` for a voice/typed submission whose messages were appended
 * under a different key — either `_draft` (no session was open) or a stale session
 * id the UI auto-restored while STT was running. Without the move, the user's
 * message would stay hidden in the old key while the new session only shows the
 * agent's reply.
 */
export function moveSessionMessages(fromSessionId: string, toSessionId: string) {
	sessionMessagesStore.update((m) => _moveMessages(m, fromSessionId, toSessionId));
}

// Resume target for navigating from history to chat with a session context.
// Set by history page before navigating to /, consumed by +page.svelte on mount.
export const resumeTargetStore = writable<any>(null);

// Active session ID that persists across SvelteKit page navigations so the
// send handler and voice recording can supplement the same session.
export const activeSessionIdStore = writable<string | null>(null);

// localStorage key recording an explicit "start a fresh conversation" intent
// that survives app restarts (set by the new-session button, cleared when the
// intent is fulfilled or abandoned). Mirrored into `newSessionIntentStore` for
// the live session.
export const NEW_ACTION_INTENT_KEY = 'haven.no_auto_restore';

/**
 * Sticky intent flag: the user explicitly asked for a NEW session (new-session
 * button). While set, NO event-driven path may auto-assign an existing session
 * to `activeSessionId` (loadSessions auto-assign, session:created, auto-restore) —
 * otherwise the next message would append to the old conversation. Cleared
 * only when the intent is fulfilled (a new session was created by the user's
 * own submission) or abandoned (explicit switch to another session).
 */
export const newSessionIntentStore = writable(false);

/**
 * Per-session token usage + cost reported by the agent. Keyed by session id.
 * Updated on every `agent:usage` event so the chat toolbar can show
 * running totals and remaining context budget.
 *
 * Shape: { [sessionId]: {
 *   promptTokens, completionTokens, totalTokens,
 *   cumulativePromptTokens, cumulativeCompletionTokens, cumulativeTotalTokens,
 *   costUsd, cumulativeCostUsd, contextWindow, model,
 *   lastUpdated: number,
 * }}
 */
export const sessionTokenStatsStore = writable<Record<string, any>>({});

/** One LLM call's usage detail row. */
export interface LlmUsage {
	id?: string;
	step_number?: number | null;
	role?: string;
	model?: string | null;
	prompt_tokens?: number;
	completion_tokens?: number;
	total_tokens?: number;
	cached_tokens?: number;
	cache_creation_tokens?: number;
	cache_miss_tokens?: number;
	cache_accounting?: 'inclusive' | 'exclusive' | 'unknown' | string;
	cache_diagnostics?: {
		mode?: string;
		key_requested?: boolean;
		system_split?: boolean;
		downgraded?: boolean;
		outcome?: string;
	};
	cost_usd?: number | null;
	has_cost?: boolean;
	duration_ms?: number | null;
	created_at?: string;
}

/**
 * Update (or insert) the token-stats entry for a session. Replaces the whole
 * session entry so stale fields don't accumulate across event variants.
 */
export function updateSessionTokenStats(sessionId: string, stats: Record<string, unknown>) {
	if (!sessionId) return;
	sessionTokenStatsStore.update((m) => ({
		...m,
		[sessionId]: { ...(m[sessionId] || {}), ...stats, lastUpdated: Date.now() },
	}));
}

/** Clear token stats for a finished/reset session. */
export function clearSessionTokenStats(sessionId: string) {
	if (!sessionId) return;
	sessionTokenStatsStore.update((m) => {
		if (!(sessionId in m)) return m;
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Restore token stats for a session from persisted backend usage counters
 * (returned by get_session_for_resume / get_last_conversation). Cumulative
 * totals are the persisted running totals; per-step and budget fields stay
 * empty until the next `agent:usage` event. `restored` marks the entry as
 * coming from persistence (a resume / reopened conversation): no further
 * `agent:usage` events may arrive, so the widget falls back to showing the
 * cumulative total instead of the per-step context count. When the session
 * predates usage persistence, `estimated` marks the restored totals as a
 * rough estimate derived from the persisted conversation text (no cost).
 * @param {string} sessionId
 * @param {object} usage - { prompt_tokens, completion_tokens, total_tokens, cached_tokens, cache_creation_tokens, cost_usd, has_cost }
 * @param {boolean} [estimated]
 */
export function restoreSessionTokenStats(
	sessionId: string,
	usage: {
		prompt_tokens?: number;
		completion_tokens?: number;
		total_tokens?: number;
		cached_tokens?: number;
		cache_creation_tokens?: number;
		cache_miss_tokens?: number;
		cost_usd?: number | null;
		has_cost?: boolean;
	},
	estimated = false,
) {
	if (!sessionId || !usage) return;
	const hasCost = !!usage.has_cost && usage.cost_usd != null;
	const prompt = usage.prompt_tokens || 0;
	const completion = usage.completion_tokens || 0;
	const cached = usage.cached_tokens || 0;
	const creation = usage.cache_creation_tokens || 0;
	const miss = usage.cache_miss_tokens || 0;
	updateSessionTokenStats(sessionId, {
		promptTokens: 0,
		completionTokens: 0,
		totalTokens: 0,
		cachedTokens: 0,
		cacheCreationTokens: 0,
		cacheMissTokens: 0,
		contextTokens: 0,
		cacheExclusive: false,
		cumulativePromptTokens: prompt,
		cumulativeCompletionTokens: completion,
		cumulativeTotalTokens: coalesceTokenTotal(
			prompt,
			completion,
			usage.total_tokens || 0,
			cached,
			creation,
		),
		cumulativeCachedTokens: cached,
		cumulativeCacheCreationTokens: creation,
		cumulativeCacheMissTokens: miss,
		costUsd: null,
		cumulativeCostUsd: hasCost ? usage.cost_usd : null,
		contextWindow: null,
		model: null,
		estimated: !!estimated,
		restored: true,
	});
}

/**
 * Per-session per-LLM-call usage detail (restored from get_session_for_resume /
 * get_last_conversation `llm_usage`), keyed by session id. Each entry is one
 * model response: { step_number, role, model, prompt_tokens,
 * completion_tokens, total_tokens, cost_usd, has_cost, duration_ms,
 * created_at }.
 * @type {import('svelte/store').Writable<Record<string, Array<object>>>}
 */
export const sessionLlmUsageStore = writable<Record<string, LlmUsage[]>>({});

/**
 * Restore the per-call usage-detail list for a session (from
 * `get_session_for_resume` / `get_last_conversation`). An EMPTY array overwrites
 * too: after a rollback truncates the usage rows the backend returns `[]`,
 * and the stale detail for discarded steps must not linger in the store
 * (mirrors restoreSessionTokenStats's unconditional overwrite). Only `undefined`
 * (backend predates the field) is ignored.
 */
export function restoreSessionLlmUsage(sessionId: string, usageList: LlmUsage[]) {
	if (!sessionId || !Array.isArray(usageList)) return;
	sessionLlmUsageStore.update((m) => ({ ...m, [sessionId]: usageList }));
}

/**
 * Append one live `agent:usage` call onto the per-session detail list so
 * tool-card token chips update during the run (not only after resume restore).
 */
export function appendSessionLlmUsage(sessionId: string, entry: LlmUsage) {
	if (!sessionId) return;
	sessionLlmUsageStore.update((m) => {
		const prev = m[sessionId] || [];
		return { ...m, [sessionId]: [...prev, entry] };
	});
}

/** Clear per-call usage detail for a finished/reset session. */
export function clearSessionLlmUsage(sessionId: string) {
	if (!sessionId) return;
	sessionLlmUsageStore.update((m) => {
		if (!(sessionId in m)) return m;
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Reconstruct `total` when a provider omitted it. Matches
 * `Usage::normalize`: inclusive `prompt + completion`, plus an explicitly
 * exclusive cache read/write bucket. Unknown legacy rows never guess from
 * cache token values.
 * @param {number} [prompt]
 * @param {number} [completion]
 * @param {number} [total]
 * @param {number} [cached]
 * @param {number} [creation]
 */
export function coalesceTokenTotal(
	prompt = 0,
	completion = 0,
	total = 0,
	cached = 0,
	creation = 0,
	cacheAccounting: string = 'unknown',
) {
	if (total) return total;
	const extra = cacheAccounting === 'exclusive' ? cached + creation : 0;
	return prompt + completion + extra || 0;
}

/**
 * Calculate a session's cache-hit rate from per-call provider contracts.
 * `prompt_tokens` already contains cache reads for inclusive providers, while
 * exclusive providers report them beside prompt tokens. Unknown legacy rows
 * return null rather than silently using an incorrect aggregate denominator.
 */
export function cumulativeCacheHitRatePercent(calls: LlmUsage[]): number | null {
	if (
		!calls.length ||
		calls.some((call) => !['inclusive', 'exclusive'].includes(call.cache_accounting || 'unknown'))
	) {
		return null;
	}
	let cached = 0;
	let eligibleInput = 0;
	for (const call of calls) {
		const prompt = call.prompt_tokens || 0;
		const read = call.cached_tokens || 0;
		const creation = call.cache_creation_tokens || 0;
		cached += read;
		eligibleInput +=
			call.cache_accounting === 'exclusive' ? prompt + read + creation : prompt;
	}
	if (!cached || !eligibleInput) return null;
	return Math.min(100, (cached / eligibleInput) * 100);
}

/**
 * Format a token count for compact display. Examples:
 *   123         -> "123"
 *   12_345      -> "12.3K"
 *   1_234_567   -> "1.23M"
 * @param {number} n
 * @returns {string}
 */
export function formatTokenCount(n: number) {
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
export function formatCostUsd(v: number | null | undefined) {
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
export function imageDataUrl(att: { media_type: string; data: string }) {
	return `data:${att.media_type};base64,${att.data}`;
}

/**
 * Format a message timestamp for bubble display. Messages from today show
 * the wall-clock time (matching live streaming bubbles); older messages
 * show the full `yyyy/mm/dd hh:mm:ss` so history stays navigable. Both the
 * live path (Date) and the resume path (RFC3339 `created_at` string) share
 * this helper so a merged list never mixes formats.
 * @param {Date|string|number} input
 * @returns {string}
 */
export function formatMessageTime(input: Date | string | number) {
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
export function newMessage({
	role,
	content,
	type = null,
	voice = false,
	time = null,
	attachments = [],
	idPrefix = '',
}: {
	role: string;
	content: string;
	type?: string | null;
	voice?: boolean;
	time?: string | null;
	attachments?: Array<{ media_type: string; data: string }>;
	idPrefix?: string;
}) {
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

let modelStateTimer: ReturnType<typeof setTimeout> | null = null;
export function updateModelState(state: string, opts: { idleTimeoutMs?: number } = {}) {
	const { idleTimeoutMs } = opts;
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
