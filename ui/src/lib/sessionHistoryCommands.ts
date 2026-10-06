import { invoke } from './tauri.ts';
import type {
	HistoryPageRequest,
	HistoryFilterRequest,
	SessionIdRequest,
	UpdateSessionTitleRequest,
} from './contracts/commands.ts';
import type {
	SessionHistoryRow,
	SessionLineageResponse,
	SessionListResponse,
	SessionResumeResponse,
} from './contracts/sessionHistory.ts';
import type { TauriCommandInvoke } from './contracts/generatedCommands.ts';

export type SessionHistoryInvoker = TauriCommandInvoke;

/** List the current in-memory session summaries for the chat shell. */
export function listSessions(): Promise<SessionListResponse> {
	return invoke('get_sessions');
}

/** Load the parent and direct child sessions shown in the active session menu. */
export function getSessionLineage(request: SessionIdRequest): Promise<SessionLineageResponse> {
	return invoke('get_session_lineage', request);
}

/** Load a recent persisted history page for the compact chat session switcher. */
export function getHistory(request: HistoryPageRequest): Promise<SessionHistoryRow[]> {
	return invoke('get_history', request);
}

/** Load the persisted history page using the existing flat Tauri arguments. */
export function searchHistoryFiltered(request: HistoryFilterRequest): Promise<SessionHistoryRow[]> {
	return invoke('search_history_filtered', request);
}

/** Load the durable projection used by session resume and transcript reload. */
export function getSessionForResume(
	request: SessionIdRequest,
	invokeCommand: SessionHistoryInvoker = invoke,
): Promise<SessionResumeResponse> {
	return invokeCommand('get_session_for_resume', request);
}

/** Load the most recent persisted conversation for startup restore. */
export function getLastConversation(): Promise<SessionResumeResponse | null> {
	return invoke('get_last_conversation');
}

/** Reopen one persisted session for follow-up input. */
export function reopenSession(request: SessionIdRequest): Promise<void> {
	return invoke('reopen_session', request);
}

/** Delete one persisted session and release its runtime state. */
export function deleteSession(request: SessionIdRequest): Promise<void> {
	return invoke('delete_session', request);
}

/** Clear all persisted session history. */
export function clearHistory(): Promise<number> {
	return invoke('clear_history');
}

/** Rename one persisted session. */
export function updateSessionTitle(request: UpdateSessionTitleRequest): Promise<void> {
	return invoke('update_session_title', request);
}
