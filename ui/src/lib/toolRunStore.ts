import { writable } from 'svelte/store';
import logger from '$lib/logger.ts';
import { reportError } from '$lib/errorHandling.ts';
import { cancelToolRunCommand, listToolRunHistory, listToolRunRows } from './toolRunCommands.ts';
import { type ToolRunKind, type ToolRunPayload } from './contracts/toolRun.ts';
import { appSessionReducer, backgroundToolRunResultContent } from './sessionReducer.ts';

/**
 * ToolRun registry (background ToolRuns + pending scheduled ToolRuns). Both
 * toolRun kinds share the same normalized id and lifecycle projection.
 */
type ToolRunEntry = ToolRunPayload;
export const toolRunStore = writable<Record<string, ToolRunEntry>>({});
/** Bounded per-session cache for timeline history and lifecycle events. */
export const sessionToolRunStore = writable<Record<string, Record<string, ToolRunEntry>>>({});

/** Cap terminal entries so a long session cannot grow the store unbounded. */
const TOOL_RUN_STORE_MAX = 64;
const SESSION_TOOL_RUN_MAX = 200;
const SESSION_TOOL_RUN_CACHE_MAX = 16;

// Only the newest reconciliation may replace the live board. This prevents a
// slow older list_tool_runs response from overwriting lifecycle events or the
// result of a newer refresh.
let toolRunRefreshRequest = 0;
// Lifecycle events may arrive while list_tool_runs is in flight. A refresh may
// replace the board only when no event has changed it since that request began.
let toolRunStateVersion = 0;
const sessionToolRunRefreshRequests = new Map<string, number>();
const sessionToolRunVersions = new Map<string, number>();
let activeTimelineSessionId: string | null = null;

/** Live board rows that must never be evicted to make room for history. */
function isLiveToolRunRow(entry: ToolRunEntry) {
	return entry.status === 'waiting' || entry.status === 'running';
}

function trimToolRunStore(entries: Record<string, ToolRunEntry>) {
	const ids = Object.keys(entries);
	if (ids.length <= TOOL_RUN_STORE_MAX) return entries;
	const excess = ids.length - TOOL_RUN_STORE_MAX;
	const victims = ids.filter((id) => !isLiveToolRunRow(entries[id]));
	let removed = 0;
	for (const id of victims) {
		if (removed >= excess) break;
		delete entries[id];
		removed++;
	}
	return entries;
}

function toolRunRecency(toolRun: ToolRunEntry): string {
	return toolRun.finishedAt || toolRun.startedAt || toolRun.dueAt || '';
}

function trimSessionToolRuns(entries: Record<string, ToolRunEntry>) {
	const newest = Object.values(entries)
		.sort(
			(left, right) =>
				toolRunRecency(right).localeCompare(toolRunRecency(left)) || right.id.localeCompare(left.id),
		)
		.slice(0, SESSION_TOOL_RUN_MAX);
	return Object.fromEntries(newest.map((entry) => [entry.id, entry]));
}

function touchSessionToolRunCache(
	current: Record<string, Record<string, ToolRunEntry>>,
	sessionId: string,
	toolRuns: Record<string, ToolRunEntry>,
) {
	const next = { ...current };
	delete next[sessionId];
	next[sessionId] = trimSessionToolRuns(toolRuns);
	while (Object.keys(next).length > SESSION_TOOL_RUN_CACHE_MAX) {
		const victim = Object.keys(next).find((key) => key !== activeTimelineSessionId);
		if (!victim) break;
		delete next[victim];
	}
	for (const key of sessionToolRunVersions.keys()) {
		if (!(key in next)) sessionToolRunVersions.delete(key);
	}
	for (const key of sessionToolRunRefreshRequests.keys()) {
		if (!(key in next)) sessionToolRunRefreshRequests.delete(key);
	}
	return next;
}

/** Pin the visible conversation's cache entry until the user switches away. */
export function setActiveSessionToolRun(sessionId: string | null) {
	activeTimelineSessionId = sessionId;
	if (!sessionId) return;
	sessionToolRunStore.update((current) =>
		touchSessionToolRunCache(current, sessionId, current[sessionId] || {}),
	);
}

/** Keep a lifecycle event in the owning session's timeline cache. */
export function upsertSessionToolRun(payload: ToolRunPayload) {
	if (!payload.id || !payload.sessionId) return;
	sessionToolRunVersions.set(
		payload.sessionId,
		(sessionToolRunVersions.get(payload.sessionId) || 0) + 1,
	);
	sessionToolRunStore.update((current) => {
		const entries = current[payload.sessionId!] || {};
		return touchSessionToolRunCache(current, payload.sessionId!, {
			...entries,
			[payload.id]: { ...entries[payload.id], ...payload },
		});
	});
}

/** Hydrate terminal ToolRuns for one conversation after switching or restart. */
export async function refreshSessionToolRuns(sessionId: string) {
	if (!sessionId) return;
	const requestId = (sessionToolRunRefreshRequests.get(sessionId) || 0) + 1;
	sessionToolRunRefreshRequests.set(sessionId, requestId);
	const stateVersion = sessionToolRunVersions.get(sessionId) || 0;
	try {
		const rows = await listToolRunHistory(undefined, SESSION_TOOL_RUN_MAX, sessionId);
		if (sessionToolRunRefreshRequests.get(sessionId) !== requestId) return;
		sessionToolRunStore.update((current) => {
			const nextRows: Record<string, ToolRunEntry> = {};
			for (const row of rows) {
				if (row.sessionId === sessionId) nextRows[row.id] = row;
			}
			// Keep events received after the DB read began; they may be newer than
			// the command response or not persisted yet.
			if ((sessionToolRunVersions.get(sessionId) || 0) !== stateVersion) {
				Object.assign(nextRows, current[sessionId] || {});
			}
			return touchSessionToolRunCache(current, sessionId, nextRows);
		});
	} catch (error) {
		reportError(error, {
			context: 'toolRunStore',
			message: '加载会话任务历史失败',
			notify: false,
		});
	}
}

export function upsertToolRun(payload: ToolRunPayload) {
	const key = payload.id;
	if (!key) return;
	toolRunStateVersion++;
	toolRunStore.update((current) => {
		const prev = current[key];
		const next: ToolRunEntry = {
			...prev,
			...payload,
		};
		return trimToolRunStore({ ...current, [key]: next });
	});
}

/** Drop a ToolRun removed from the live board by a terminal lifecycle event. */
export function removeToolRun(id: string) {
	if (!id) return;
	toolRunStateVersion++;
	toolRunStore.update((current) => {
		if (!(id in current)) return current;
		const next = { ...current };
		delete next[id];
		return next;
	});
}

export async function refreshToolRuns() {
	const requestId = ++toolRunRefreshRequest;
	const stateVersion = toolRunStateVersion;
	try {
		const rows = await listToolRunRows();
		if (!rows) return;
		if (requestId !== toolRunRefreshRequest) return;
		if (stateVersion !== toolRunStateVersion) return;
		// Missing rows were removed server-side, so replace the registry instead
		// of leaving stale lifecycle entries in the UI.
		toolRunStore.update((current) => {
			const next: Record<string, ToolRunEntry> = {};
			for (const row of rows) {
				if (!row) {
					logger.warn('toolRunStore', 'Dropping malformed toolRun board row');
					continue;
				}
				const key = row.id;
				const merged: ToolRunEntry = {
					...(current[key] || {}),
					...row,
					id: key,
				};
				// Terminal background ToolRuns do not belong in the live registry.
				if (merged.kind === 'background' && merged.status && merged.status !== 'running') {
					continue;
				}
				next[key] = merged;
			}
			return trimToolRunStore(next);
		});
	} catch (error) {
		reportError(error, {
			context: 'toolRunStore',
			message: '刷新任务列表失败',
			notify: false,
		});
	}
}

export async function cancelToolRun(id: string, kind: ToolRunKind = 'background') {
	return cancelToolRunCommand({ toolRunId: id, kind });
}

/**
 * Persist a terminal background ToolRun payload onto any tool cards still
 * bound via `toolRunId`, then clear that bind so the card cannot fall back to
 * the original running observation.
 */
export function finalizeBackgroundToolRunMessages(payload: ToolRunPayload) {
	const content = backgroundToolRunResultContent(payload);
	if (!content) return;
	appSessionReducer.dispatch({
		type: 'session/background-result',
		sessionId: payload.sessionId,
		toolRunId: payload.id,
		content,
	});
}
