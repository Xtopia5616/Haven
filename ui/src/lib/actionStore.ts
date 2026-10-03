import { writable } from 'svelte/store';
import logger from '$lib/logger.ts';
import { reportError } from '$lib/errorHandling.ts';
import { cancelActionCommand, listActionHistory, listActionRows } from './actionCommands.ts';
import { type ActionKind, type ActionPayload } from './contracts/action.ts';
import { appSessionReducer, backgroundActionResultContent } from './sessionReducer.ts';

/**
 * Action registry (background actions + pending scheduled actions). Both
 * action kinds share the same normalized id and lifecycle projection.
 */
type ActionEntry = ActionPayload;
export const actionStore = writable<Record<string, ActionEntry>>({});
/** Bounded per-session cache for timeline history and lifecycle events. */
export const sessionActionStore = writable<Record<string, Record<string, ActionEntry>>>({});

/** Cap terminal entries so a long session cannot grow the store unbounded. */
const ACTION_STORE_MAX = 64;
const SESSION_ACTION_MAX = 200;
const SESSION_ACTION_CACHE_MAX = 16;

// Only the newest reconciliation may replace the live board. This prevents a
// slow older list_actions response from overwriting lifecycle events or the
// result of a newer refresh.
let actionRefreshRequest = 0;
// Lifecycle events may arrive while list_actions is in flight. A refresh may
// replace the board only when no event has changed it since that request began.
let actionStateVersion = 0;
const sessionActionRefreshRequests = new Map<string, number>();
const sessionActionVersions = new Map<string, number>();
let activeTimelineSessionId: string | null = null;

/** Live board rows that must never be evicted to make room for history. */
function isLiveActionRow(entry: ActionEntry) {
	return entry.status === 'waiting' || entry.status === 'running';
}

function trimActionStore(entries: Record<string, ActionEntry>) {
	const ids = Object.keys(entries);
	if (ids.length <= ACTION_STORE_MAX) return entries;
	const excess = ids.length - ACTION_STORE_MAX;
	const victims = ids.filter((id) => !isLiveActionRow(entries[id]));
	let removed = 0;
	for (const id of victims) {
		if (removed >= excess) break;
		delete entries[id];
		removed++;
	}
	return entries;
}

function actionRecency(action: ActionEntry): string {
	return action.finishedAt || action.startedAt || action.dueAt || '';
}

function trimSessionActions(entries: Record<string, ActionEntry>) {
	const newest = Object.values(entries)
		.sort(
			(left, right) =>
				actionRecency(right).localeCompare(actionRecency(left)) || right.id.localeCompare(left.id),
		)
		.slice(0, SESSION_ACTION_MAX);
	return Object.fromEntries(newest.map((entry) => [entry.id, entry]));
}

function touchSessionActionCache(
	current: Record<string, Record<string, ActionEntry>>,
	sessionId: string,
	actions: Record<string, ActionEntry>,
) {
	const next = { ...current };
	delete next[sessionId];
	next[sessionId] = trimSessionActions(actions);
	while (Object.keys(next).length > SESSION_ACTION_CACHE_MAX) {
		const victim = Object.keys(next).find((key) => key !== activeTimelineSessionId);
		if (!victim) break;
		delete next[victim];
	}
	for (const key of sessionActionVersions.keys()) {
		if (!(key in next)) sessionActionVersions.delete(key);
	}
	for (const key of sessionActionRefreshRequests.keys()) {
		if (!(key in next)) sessionActionRefreshRequests.delete(key);
	}
	return next;
}

/** Pin the visible conversation's cache entry until the user switches away. */
export function setActiveSessionAction(sessionId: string | null) {
	activeTimelineSessionId = sessionId;
	if (!sessionId) return;
	sessionActionStore.update((current) =>
		touchSessionActionCache(current, sessionId, current[sessionId] || {}),
	);
}

/** Keep a lifecycle event in the owning session's timeline cache. */
export function upsertSessionAction(payload: ActionPayload) {
	if (!payload.id || !payload.sessionId) return;
	sessionActionVersions.set(
		payload.sessionId,
		(sessionActionVersions.get(payload.sessionId) || 0) + 1,
	);
	sessionActionStore.update((current) => {
		const entries = current[payload.sessionId!] || {};
		return touchSessionActionCache(current, payload.sessionId!, {
			...entries,
			[payload.id]: { ...entries[payload.id], ...payload },
		});
	});
}

/** Hydrate terminal actions for one conversation after switching or restart. */
export async function refreshSessionActions(sessionId: string) {
	if (!sessionId) return;
	const requestId = (sessionActionRefreshRequests.get(sessionId) || 0) + 1;
	sessionActionRefreshRequests.set(sessionId, requestId);
	const stateVersion = sessionActionVersions.get(sessionId) || 0;
	try {
		const rows = await listActionHistory(undefined, SESSION_ACTION_MAX, sessionId);
		if (sessionActionRefreshRequests.get(sessionId) !== requestId) return;
		sessionActionStore.update((current) => {
			const nextRows: Record<string, ActionEntry> = {};
			for (const row of rows) {
				if (row.sessionId === sessionId) nextRows[row.id] = row;
			}
			// Keep events received after the DB read began; they may be newer than
			// the command response or not persisted yet.
			if ((sessionActionVersions.get(sessionId) || 0) !== stateVersion) {
				Object.assign(nextRows, current[sessionId] || {});
			}
			return touchSessionActionCache(current, sessionId, nextRows);
		});
	} catch (error) {
		reportError(error, {
			context: 'actionStore',
			message: '加载会话任务历史失败',
			notify: false,
		});
	}
}

export function upsertAction(payload: ActionPayload) {
	const key = payload.id;
	if (!key) return;
	actionStateVersion++;
	actionStore.update((current) => {
		const prev = current[key];
		const next: ActionEntry = {
			...prev,
			...payload,
		};
		return trimActionStore({ ...current, [key]: next });
	});
}

/** Drop an action removed from the live board by a terminal lifecycle event. */
export function removeAction(id: string) {
	if (!id) return;
	actionStateVersion++;
	actionStore.update((current) => {
		if (!(id in current)) return current;
		const next = { ...current };
		delete next[id];
		return next;
	});
}

export async function refreshActions() {
	const requestId = ++actionRefreshRequest;
	const stateVersion = actionStateVersion;
	try {
		const rows = await listActionRows();
		if (!rows) return;
		if (requestId !== actionRefreshRequest) return;
		if (stateVersion !== actionStateVersion) return;
		// Missing rows were removed server-side, so replace the registry instead
		// of leaving stale lifecycle entries in the UI.
		actionStore.update((current) => {
			const next: Record<string, ActionEntry> = {};
			for (const row of rows) {
				if (!row) {
					logger.warn('actionStore', 'Dropping malformed action board row');
					continue;
				}
				const key = row.id;
				const merged: ActionEntry = {
					...(current[key] || {}),
					...row,
					id: key,
				};
				// Terminal background actions do not belong in the live registry.
				if (merged.kind === 'background' && merged.status && merged.status !== 'running') {
					continue;
				}
				next[key] = merged;
			}
			return trimActionStore(next);
		});
	} catch (error) {
		reportError(error, {
			context: 'actionStore',
			message: '刷新任务列表失败',
			notify: false,
		});
	}
}

export async function cancelAction(id: string, kind: ActionKind = 'background') {
	return cancelActionCommand({ actionId: id, kind });
}

/**
 * Persist a terminal background-action payload onto any tool cards still
 * bound via `actionId`, then clear that bind so the card cannot fall back to
 * the original running observation.
 */
export function finalizeBackgroundActionMessages(payload: ActionPayload) {
	const content = backgroundActionResultContent(payload);
	if (!content) return;
	appSessionReducer.dispatch({
		type: 'session/background-result',
		sessionId: payload.sessionId,
		actionId: payload.id,
		content,
	});
}
