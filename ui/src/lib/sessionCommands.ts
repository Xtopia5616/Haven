import { invoke } from './tauri.ts';
import type {
	ContinueSessionRequest,
	DeleteSessionRequest,
	EndSessionRequest,
	GetSessionForResumeRequest,
	GetSessionLineageRequest,
	InterruptSessionRequest,
	RollbackSessionRequest,
	ReopenSessionRequest,
	SessionHistoryFilterRequest,
	SessionHistoryPageRequest,
	UpdateSessionTitleRequest,
} from './contracts/commands.ts';
import type {
	SessionHistoryRow,
	SessionLineageResponse,
	RuntimeSessionListResponse,
	SessionResumeResponse,
} from './contracts/sessionHistory.ts';
import {
	parseRuntimeSessionListResponse,
	parseSessionHistoryRows,
	parseSessionLineageResponse,
	parseSessionResumeResponse,
} from './contracts/sessionHistory.ts';

/** List the current in-memory session summaries for the chat shell. */
export async function listRuntimeSessions(): Promise<RuntimeSessionListResponse> {
	const value: unknown = await invoke('list_runtime_sessions');
	return parseRuntimeSessionListResponse(value);
}

/** Load the parent and direct child sessions shown in the active session menu. */
export async function getSessionLineage(
	request: GetSessionLineageRequest,
): Promise<SessionLineageResponse> {
	const value: unknown = await invoke('get_session_lineage', request);
	return parseSessionLineageResponse(value);
}

/** Load a recent persisted history page for the compact chat session switcher. */
export async function listSessionHistory(
	request: SessionHistoryPageRequest,
): Promise<SessionHistoryRow[]> {
	const value: unknown = await invoke('list_session_history', request);
	return parseSessionHistoryRows(value);
}

/** Load the persisted history page using the existing flat Tauri arguments. */
export async function searchSessionHistoryFiltered(
	request: SessionHistoryFilterRequest,
): Promise<SessionHistoryRow[]> {
	const value: unknown = await invoke('search_session_history_filtered', request);
	return parseSessionHistoryRows(value);
}

/** Load the durable projection used by session resume and transcript reload. */
export async function getSessionForResume(
	request: GetSessionForResumeRequest,
): Promise<SessionResumeResponse> {
	const value: unknown = await invoke('get_session_for_resume', request);
	return parseSessionResumeResponse(value);
}

/** Load the most recent persisted session projection for startup resume. */
export async function getLatestSessionForResume(): Promise<SessionResumeResponse | null> {
	const value: unknown = await invoke('get_latest_session_for_resume');
	return value === null ? null : parseSessionResumeResponse(value);
}

/** Reopen one persisted session for follow-up input. */
export function reopenSession(request: ReopenSessionRequest): Promise<void> {
	return invoke('reopen_session', request);
}

/** Delete one persisted session and release its runtime state. */
export function deleteSession(request: DeleteSessionRequest): Promise<void> {
	return invoke('delete_session', request);
}

/** Clear all persisted session history. */
export function deleteAllSessions(): Promise<number> {
	return invoke('delete_all_sessions');
}

/** Rename one persisted session. */
export function updateSessionTitle(request: UpdateSessionTitleRequest): Promise<void> {
	return invoke('update_session_title', request);
}

/** Roll back a session's durable transcript and runtime state. */
export function rollbackSession(request: RollbackSessionRequest): Promise<void> {
	return invoke('rollback_session', request);
}

/** End the active session through the session lifecycle boundary. */
export function endSession(request: EndSessionRequest): Promise<void> {
	return invoke('end_session', request);
}

/** Interrupt the active session's current output. */
export function interruptSession(request: InterruptSessionRequest): Promise<void> {
	return invoke('interrupt_session', request);
}

/** Continue the active session's paused or failed run. */
export function continueSession(request: ContinueSessionRequest): Promise<void> {
	return invoke('continue_session', request);
}
