import { writable } from 'svelte/store';
import logger from '$lib/logger.ts';
import { reportError } from '$lib/errorHandling.ts';
import { cancelActionCommand, listActionRows } from './actionCommands.ts';
import { type ActionKind, type ActionPayload } from './contracts/action.ts';
import { appSessionReducer, backgroundActionResultContent } from './sessionReducer.ts';

/**
 * Action registry (background actions + pending scheduled actions). Both
 * action kinds share the same normalized id and lifecycle projection.
 */
type ActionEntry = ActionPayload;
export const actionStore = writable<Record<string, ActionEntry>>({});

/** Cap terminal entries so a long session cannot grow the store unbounded. */
const ACTION_STORE_MAX = 64;

// Only the newest reconciliation may replace the live board. This prevents a
// slow older list_actions response from overwriting lifecycle events or the
// result of a newer refresh.
let actionRefreshRequest = 0;
// Lifecycle events may arrive while list_actions is in flight. A refresh may
// replace the board only when no event has changed it since that request began.
let actionStateVersion = 0;

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
