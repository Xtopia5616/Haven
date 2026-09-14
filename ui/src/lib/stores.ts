import { get, writable } from 'svelte/store';
import { invoke } from './tauri.ts';
import logger from '$lib/logger.ts';
import { mapActionPayload, type ActionKind, type ActionPayload } from './contracts/action.ts';
import type { AgentMediaPlanPayload } from './contracts/agent.ts';
import type {
	InteractionKind,
	InteractionRequest,
	InteractionStatus,
} from './contracts/app.ts';
import { sessionMessagesStore, updateSessionMessages } from './sessionMessages.ts';

export const sessionStore = writable<any[]>([]);

/**
 * The single renderer-side source of truth for ask, confirm, and scheduled
 * confirmation requests. Message bubbles and modal views are projections of
 * this store; they must not maintain separate pending queues.
 */
export const interactionStore = writable<Record<string, InteractionRequest>>({});

export function upsertInteraction(request: InteractionRequest) {
	if (!request?.id || !request.sessionId) return;
	interactionStore.update((current) => {
		const previous = current[request.id];
		if (previous && JSON.stringify(previous) === JSON.stringify(request)) return current;
		return { ...current, [request.id]: request };
	});
}

/** Replace one session's interaction projection from a renderer-safe resume DTO. */
export function hydrateInteractions(result: any) {
	const sessionId = result?.session?.id;
	if (sessionId) clearSessionInteractions(sessionId);
	for (const raw of result?.interactions || []) {
		const request: InteractionRequest = {
			id: raw.id,
			sessionId: raw.sessionId ?? raw.session_id ?? '',
			kind: raw.kind,
			status: raw.status,
			prompt: raw.prompt || '',
			options: raw.options || [],
			...(raw.toolName || raw.tool_name
				? { toolName: raw.toolName ?? raw.tool_name }
				: {}),
			...(raw.riskLevel || raw.risk_level
				? { riskLevel: raw.riskLevel ?? raw.risk_level }
				: {}),
			...(raw.summary || raw.summary === '' ? { summary: raw.summary } : {}),
			...(raw.permissionKey || raw.permission_key
				? { permissionKey: raw.permissionKey ?? raw.permission_key }
				: {}),
			...(raw.invocationStepId || raw.invocation_step_id
				? { invocationStepId: raw.invocationStepId ?? raw.invocation_step_id }
				: {}),
			...((raw.actionIndex ?? raw.action_index) != null
				? { actionIndex: raw.actionIndex ?? raw.action_index }
				: {}),
			...(raw.toolCallId || raw.tool_call_id
				? { toolCallId: raw.toolCallId ?? raw.tool_call_id }
				: {}),
			createdAt: raw.createdAt ?? raw.created_at ?? new Date().toISOString(),
			...(raw.expiresAt || raw.expires_at
				? { expiresAt: raw.expiresAt ?? raw.expires_at }
				: {}),
		};
		upsertInteraction(request);
	}
}

export function resolveInteraction(id: string, response: unknown = undefined) {
	if (!id) return;
	interactionStore.update((current) => {
		const request = current[id];
		if (!request || request.status !== 'pending') return current;
		return {
			...current,
			[id]: {
				...request,
				status: 'resolved' as InteractionStatus,
				...(response === undefined ? {} : { response }),
			},
		};
	});
}

export function removeInteraction(id: string) {
	if (!id) return;
	interactionStore.update((current) => {
		if (!(id in current)) return current;
		const next = { ...current };
		delete next[id];
		return next;
	});
}

export function clearSessionInteractions(sessionId: string, kind?: InteractionKind) {
	if (!sessionId) return;
	interactionStore.update((current) => {
		const next = Object.fromEntries(
			Object.entries(current).filter(
				([, request]) =>
					request.sessionId !== sessionId || (kind && request.kind !== kind),
			),
		);
		return Object.keys(next).length === Object.keys(current).length ? current : next;
	});
}

export function pendingInteractions(
	sessionId?: string | null,
	kind?: InteractionKind,
): InteractionRequest[] {
	return Object.values(get(interactionStore)).filter(
		(request) =>
			request.status === 'pending' &&
			(!sessionId || request.sessionId === sessionId) &&
			(!kind || request.kind === kind),
	);
}

/**
 * User-visible error reasons for sessions that failed during this app run.
 * The backend error event is already sanitized; this UI cache lets a
 * history-opened error session show the same reason without changing its
 * persisted lifecycle state just to inspect it.
 */
export const sessionErrorReasonStore = writable<Record<string, string>>({});

export function rememberSessionError(sessionId: string, reason: string) {
	const normalized = reason.trim();
	if (!sessionId || !normalized) return;
	sessionErrorReasonStore.update((reasons) => {
		if (reasons[sessionId] === normalized) return reasons;
		return { ...reasons, [sessionId]: normalized };
	});
}

export function forgetSessionError(sessionId: string) {
	if (!sessionId) return;
	sessionErrorReasonStore.update((reasons) => {
		if (!(sessionId in reasons)) return reasons;
		const next = { ...reasons };
		delete next[sessionId];
		return next;
	});
}

export function getSessionErrorReason(sessionId: string): string {
	return get(sessionErrorReasonStore)[sessionId] || '';
}

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
 * Live media-plan UI projections keyed by session. The backend also records a
 * snapshot-safe MediaPlan event in the ReAct event authority; this bounded
 * cache only keeps cards available while the current app run is visible.
 */
export const mediaPlanStore = writable<Record<string, AgentMediaPlanPayload[]>>({});

const MEDIA_PLAN_HISTORY_LIMIT = 32;

export function rememberMediaPlan(payload: AgentMediaPlanPayload) {
	if (!payload.sessionId) return;
	const key = `${payload.stepNumber}:${payload.runId}:${payload.role}`;
	mediaPlanStore.update((all) => {
		const previous = all[payload.sessionId] || [];
		const next = previous.filter(
			(plan) => `${plan.stepNumber}:${plan.runId}:${plan.role}` !== key,
		);
		return {
			...all,
			[payload.sessionId]: [...next, payload].slice(-MEDIA_PLAN_HISTORY_LIMIT),
		};
	});
}

export function clearMediaPlans(sessionId: string) {
	if (!sessionId) return;
	mediaPlanStore.update((all) => {
		if (!(sessionId in all)) return all;
		const next = { ...all };
		delete next[sessionId];
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
				// Keep terminal background rows out of the live registry so they do
				// not bloat the store or let eviction delete live running rows.
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

export type NotificationType = 'info' | 'success' | 'warning' | 'error';

export type Notification = {
	id: string;
	msg: string;
	type: NotificationType;
};

export type NotificationOptions = {
	/** Internal escape hatch for reportError, which owns the log entry. */
	logError?: boolean;
};

export const notificationStore = writable<Notification[]>([]);

export const NOTIFICATION_DURATIONS: Record<NotificationType, number> = {
	info: 3000,
	success: 3000,
	warning: 4000,
	error: 5000,
};

let notificationSeq = 0;

export function addNotification(
	msg: string,
	type: NotificationType = 'info',
	duration = NOTIFICATION_DURATIONS[type],
	options: NotificationOptions = {},
) {
	if (type === 'error' && options.logError !== false) {
		logger.error('notification', msg);
	}
	if (type === 'error') {
		// Error toasts are also mirrored into the Rust file log so the Settings
		// log viewer contains the same user-visible failure. This is best effort:
		// the notification must remain usable when running outside Tauri or when
		// the diagnostic command is unavailable during an upgrade.
		void invoke('log_frontend_error', { message: msg }).catch(() => {});
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
 * Build a `data:` URL from a media attachment ({ media_type, data } where
 * data is base64 without the prefix). Shared by the input area previews and
 * ChatBubble rendering.
 * @param {{ media_type: string, data: string }} att
 */
export function mediaDataUrl(att: { media_type: string; data: string }) {
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

// Presentation-level status for the currently selected conversation. The chat
// route owns the session/action context; the shell consumes this single value
// so the titlebar is the only lifecycle indicator shown to the user.
export const activeConversationStatusStore = writable('就绪');

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
