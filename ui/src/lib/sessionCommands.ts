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

/** List the current in-memory session summaries for the chat shell. */
export function listRuntimeSessions(): Promise<RuntimeSessionListResponse> {
	return invoke('list_runtime_sessions');
}

/** Load the parent and direct child sessions shown in the active session menu. */
export function getSessionLineage(
	request: GetSessionLineageRequest,
): Promise<SessionLineageResponse> {
	return invoke('get_session_lineage', request);
}

/** Load a recent persisted history page for the compact chat session switcher. */
export function listSessionHistory(
	request: SessionHistoryPageRequest,
): Promise<SessionHistoryRow[]> {
	return invoke('list_session_history', request);
}

/** Load the persisted history page using the existing flat Tauri arguments. */
export function searchSessionHistoryFiltered(
	request: SessionHistoryFilterRequest,
): Promise<SessionHistoryRow[]> {
	return invoke('search_session_history_filtered', request);
}

/** Load the durable projection used by session resume and transcript reload. */
export function getSessionForResume(
	request: GetSessionForResumeRequest,
): Promise<SessionResumeResponse> {
	return invoke('get_session_for_resume', request);
}

/** Load the most recent persisted session projection for startup resume. */
export function getLatestSessionForResume(): Promise<SessionResumeResponse | null> {
	return invoke('get_latest_session_for_resume');
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
