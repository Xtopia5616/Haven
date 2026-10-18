import { derived, writable } from 'svelte/store';
import logger from '$lib/logger.ts';
import { reportError } from '$lib/errorHandling.ts';
import { cancelToolRunCommand, listToolRunHistory, listToolRunRows } from './toolRunCommands.ts';
import { type ToolRunPayload } from './contracts/toolRun.ts';
import { appSessionReducer, backgroundToolRunResultContent } from './sessionReducer.ts';

interface ToolRunState {
	payloadsById: Record<string, ToolRunPayload>;
	/** Ordered live-board identity index. Payloads live only in payloadsById. */
	liveBoardIds: string[];
	/** Per-session ordered history identity indexes. */
	sessionHistoryIds: Record<string, string[]>;
	/** LRU order for the bounded session-history index cache. */
	sessionCacheIds: string[];
}

const EMPTY_TOOL_RUN_STATE: ToolRunState = {
	payloadsById: {},
	liveBoardIds: [],
	sessionHistoryIds: {},
	sessionCacheIds: [],
};

/**
 * The only ToolRun payload owner. The live board and session timeline expose
 * derived views over this map and keep only their own bounded identity indexes.
 */
const toolRunState = writable<ToolRunState>(EMPTY_TOOL_RUN_STATE);
export const toolRunStore = derived(toolRunState, ({ payloadsById, liveBoardIds }) =>
	Object.fromEntries(
		liveBoardIds.flatMap((id) => {
			const payload = payloadsById[id];
			return payload ? [[id, payload] as const] : [];
		}),
	),
);
export const sessionToolRunStore = derived(
	toolRunState,
	({ payloadsById, sessionHistoryIds }) =>
		Object.fromEntries(
			Object.entries(sessionHistoryIds).map(([sessionId, ids]) => [
				sessionId,
				Object.fromEntries(
					ids.flatMap((id) => {
						const payload = payloadsById[id];
						return payload ? [[id, payload] as const] : [];
					}),
				),
			]),
		),
);

/** Cap terminal entries so a long live board cannot grow the store unbounded. */
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
function isLiveToolRunRow(entry: ToolRunPayload) {
	return entry.status === 'running';
}

function toolRunRecency(toolRun: ToolRunPayload): string {
	return toolRun.finishedAt || toolRun.startedAt || '';
}

function trimLiveBoardIds(ids: string[], payloadsById: Record<string, ToolRunPayload>) {
	if (ids.length <= TOOL_RUN_STORE_MAX) return ids;
	const excess = ids.length - TOOL_RUN_STORE_MAX;
	const victims = ids.filter((id) => !isLiveToolRunRow(payloadsById[id]));
	const removed = new Set(victims.slice(0, excess));
	return ids.filter((id) => !removed.has(id));
}

function trimSessionHistoryIds(ids: string[], payloadsById: Record<string, ToolRunPayload>) {
	return [...ids]
		.sort((leftId, rightId) => {
			const left = payloadsById[leftId];
			const right = payloadsById[rightId];
			if (!left || !right) return left ? -1 : right ? 1 : leftId.localeCompare(rightId);
			return (
				toolRunRecency(right).localeCompare(toolRunRecency(left)) ||
				right.toolRunId.localeCompare(left.toolRunId)
			);
		})
		.slice(0, SESSION_TOOL_RUN_MAX);
}

function pruneUnindexedPayloads(state: ToolRunState): ToolRunState {
	const indexedIds = new Set(state.liveBoardIds);
	for (const ids of Object.values(state.sessionHistoryIds)) {
		for (const id of ids) indexedIds.add(id);
	}
	const payloadsById = Object.fromEntries(
		Object.entries(state.payloadsById).filter(([id]) => indexedIds.has(id)),
	);
	return { ...state, payloadsById };
}

function pruneSessionRequestState(sessionHistoryIds: Record<string, string[]>) {
	for (const key of sessionToolRunVersions.keys()) {
		if (!(key in sessionHistoryIds)) sessionToolRunVersions.delete(key);
	}
	for (const key of sessionToolRunRefreshRequests.keys()) {
		if (!(key in sessionHistoryIds)) sessionToolRunRefreshRequests.delete(key);
	}
}

function touchSessionHistory(
	state: ToolRunState,
	sessionId: string,
	ids: string[],
): ToolRunState {
	const sessionHistoryIds = {
		...state.sessionHistoryIds,
		[sessionId]: trimSessionHistoryIds(ids, state.payloadsById),
	};
	let sessionCacheIds = state.sessionCacheIds.filter((id) => id !== sessionId);
	sessionCacheIds.push(sessionId);
	while (sessionCacheIds.length > SESSION_TOOL_RUN_CACHE_MAX) {
		const victim = sessionCacheIds.find((id) => id !== activeTimelineSessionId);
		if (!victim) break;
		sessionCacheIds = sessionCacheIds.filter((id) => id !== victim);
		delete sessionHistoryIds[victim];
	}
	pruneSessionRequestState(sessionHistoryIds);
	return { ...state, sessionHistoryIds, sessionCacheIds };
}

function mergePayload(state: ToolRunState, payload: ToolRunPayload): ToolRunState {
	const previous = state.payloadsById[payload.toolRunId];
	return {
		...state,
		payloadsById: {
			...state.payloadsById,
			[payload.toolRunId]: { ...previous, ...payload },
		},
	};
}

/** Pin the visible session's index entry until the user switches away. */
export function setActiveSessionToolRun(sessionId: string | null) {
	activeTimelineSessionId = sessionId;
	if (!sessionId) return;
	toolRunState.update((current) =>
		pruneUnindexedPayloads(
			touchSessionHistory(current, sessionId, current.sessionHistoryIds[sessionId] || []),
		),
	);
}

/** Keep a lifecycle event in the owning session's timeline identity index. */
export function upsertSessionToolRun(payload: ToolRunPayload) {
	if (!payload.toolRunId || !payload.sessionId) return;
	// Session lifecycle events update the same payload map as the live board;
	// fence any board hydration that began before this newer event arrived.
	toolRunStateVersion++;
	const sessionId = payload.sessionId;
	sessionToolRunVersions.set(sessionId, (sessionToolRunVersions.get(sessionId) || 0) + 1);
	toolRunState.update((current) => {
		const merged = mergePayload(current, payload);
		const ids = merged.sessionHistoryIds[sessionId] || [];
		return pruneUnindexedPayloads(
			touchSessionHistory(
				merged,
				sessionId,
				ids.includes(payload.toolRunId) ? ids : [...ids, payload.toolRunId],
			),
		);
	});
}

/** Hydrate terminal ToolRuns for one session after switching or restart. */
export async function refreshSessionToolRuns(sessionId: string) {
	if (!sessionId) return;
	const requestId = (sessionToolRunRefreshRequests.get(sessionId) || 0) + 1;
	sessionToolRunRefreshRequests.set(sessionId, requestId);
	const stateVersion = sessionToolRunVersions.get(sessionId) || 0;
	try {
		const rows = await listToolRunHistory(SESSION_TOOL_RUN_MAX, sessionId);
		if (sessionToolRunRefreshRequests.get(sessionId) !== requestId) return;
		const preserveEvents = (sessionToolRunVersions.get(sessionId) || 0) !== stateVersion;
		toolRunState.update((current) => {
			let next = current;
			const refreshedIds: string[] = [];
			const eventIds = new Set(current.sessionHistoryIds[sessionId] || []);
			for (const row of rows) {
				if (row.sessionId !== sessionId) continue;
				if (!preserveEvents || !eventIds.has(row.toolRunId)) {
					next = mergePayload(next, row);
				}
				if (!refreshedIds.includes(row.toolRunId)) refreshedIds.push(row.toolRunId);
			}
			const ids = preserveEvents
				? [...new Set([...refreshedIds, ...(current.sessionHistoryIds[sessionId] || [])])]
				: refreshedIds;
			return pruneUnindexedPayloads(touchSessionHistory(next, sessionId, ids));
		});
	} catch (error) {
		reportError(error, {
			context: 'toolRunStore',
			message: '加载任务历史失败',
			notify: false,
		});
	}
}

/** Insert or update one payload and add it to the live-board identity index. */
export function upsertToolRun(payload: ToolRunPayload) {
	const key = payload.toolRunId;
	if (!key) return;
	toolRunStateVersion++;
	toolRunState.update((current) => {
		const merged = mergePayload(current, payload);
		const ids = current.liveBoardIds.includes(key) ? current.liveBoardIds : [...current.liveBoardIds, key];
		return pruneUnindexedPayloads({
			...merged,
			liveBoardIds: trimLiveBoardIds(ids, merged.payloadsById),
		});
	});
}

/** Drop a ToolRun from the live board while preserving any session index. */
export function removeToolRun(toolRunId: string) {
	if (!toolRunId) return;
	toolRunStateVersion++;
	toolRunState.update((current) => {
		if (!current.liveBoardIds.includes(toolRunId)) return current;
		return pruneUnindexedPayloads({
			...current,
			liveBoardIds: current.liveBoardIds.filter((id) => id !== toolRunId),
		});
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
		toolRunState.update((current) => {
			let next = current;
			const liveBoardIds: string[] = [];
			for (const row of rows) {
				if (!row) {
					logger.warn('toolRunStore', 'Dropping malformed toolRun board row');
					continue;
				}
				next = mergePayload(next, { ...row, toolRunId: row.toolRunId });
				const merged = next.payloadsById[row.toolRunId];
				// Terminal background ToolRuns do not belong in the live registry.
				if (merged.kind === 'background' && merged.status && merged.status !== 'running') {
					continue;
				}
				if (!liveBoardIds.includes(row.toolRunId)) liveBoardIds.push(row.toolRunId);
			}
			return pruneUnindexedPayloads({
				...next,
				liveBoardIds: trimLiveBoardIds(liveBoardIds, next.payloadsById),
			});
		});
	} catch (error) {
		reportError(error, {
			context: 'toolRunStore',
			message: '刷新任务列表失败',
			notify: false,
		});
	}
}

/** @internal Store reset hook for isolated UI tests. */
export function resetToolRunStoreForTest() {
	toolRunRefreshRequest++;
	toolRunStateVersion = 0;
	sessionToolRunRefreshRequests.clear();
	sessionToolRunVersions.clear();
	activeTimelineSessionId = null;
	toolRunState.set(EMPTY_TOOL_RUN_STATE);
}

export async function cancelToolRun(toolRunId: string) {
	return cancelToolRunCommand({ toolRunId });
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
		toolRunId: payload.toolRunId,
		content,
	});
}
