import { invoke } from './tauri.ts';
import type { TauriCommandRequest } from './contracts/generatedCommands.ts';
import { parseTranscriptSubmissionResult, type TranscriptSubmissionResultView } from './contracts/transcriptSubmissionResult.ts';
import { parseCountResponse } from './contracts/count.ts';
import { isCanonicalEntityId } from './contracts/wireGuards.ts';
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
	request: TauriCommandRequest<'get_session_lineage'>,
): Promise<SessionLineageResponse> {
	const value: unknown = await invoke('get_session_lineage', request);
	return parseSessionLineageResponse(value, request.sessionId);
}

/** Load a recent persisted history page for the compact chat session switcher. */
export async function listSessionHistory(
	request: TauriCommandRequest<'list_session_history'>,
): Promise<SessionHistoryRow[]> {
	const value: unknown = await invoke('list_session_history', request);
	return parseSessionHistoryRows(value);
}

/** Load the persisted history page using the existing flat Tauri arguments. */
export async function searchSessionHistoryFiltered(
	request: TauriCommandRequest<'search_session_history_filtered'>,
): Promise<SessionHistoryRow[]> {
	const value: unknown = await invoke('search_session_history_filtered', request);
	return parseSessionHistoryRows(value);
}

/** Load the durable projection used by session resume and transcript reload. */
export async function getSessionForResume(
	request: TauriCommandRequest<'get_session_for_resume'>,
): Promise<SessionResumeResponse> {
	const value: unknown = await invoke('get_session_for_resume', request);
	return parseSessionResumeResponse(value, request.sessionId);
}

/** Load the most recent persisted session projection for startup resume. */
export async function getLatestSessionForResume(): Promise<SessionResumeResponse | null> {
	const value: unknown = await invoke('get_latest_session_for_resume');
	return value === null ? null : parseSessionResumeResponse(value);
}

/** Submit transcript text and attachments through the canonical Session ingress. */
export async function submitTranscriptCommand(
	request: TauriCommandRequest<'submit_transcript'>,
): Promise<TranscriptSubmissionResultView> {
	const value: unknown = await invoke('submit_transcript', request);
	return parseTranscriptSubmissionResult(value, request.activeSessionId);
}

/** Reopen one persisted session for follow-up input. */
export function reopenSession(request: TauriCommandRequest<'reopen_session'>): Promise<void> {
	return invoke('reopen_session', request);
}

/** Delete one persisted session and release its runtime state. */
export function deleteSession(request: TauriCommandRequest<'delete_session'>): Promise<void> {
	return invoke('delete_session', request);
}

/** Clear all persisted session history. */
export function deleteAllSessions(): Promise<number> {
	return invoke('delete_all_sessions').then((value: unknown) =>
		parseCountResponse(value, 'delete_all_sessions'),
	);
}

/** Count persisted session history through the validated numeric command boundary. */
export function countSessionHistory(): Promise<number> {
	return invoke('count_session_history').then((value: unknown) =>
		parseCountResponse(value, 'count_session_history'),
	);
}

/** Count persisted session history matching a search query. */
export function countSessionHistorySearch(
	request: TauriCommandRequest<'count_session_history_search'>,
): Promise<number> {
	return invoke('count_session_history_search', request).then((value: unknown) =>
		parseCountResponse(value, 'count_session_history_search'),
	);
}

/** Rename one persisted session. */
export function updateSessionTitle(
	request: TauriCommandRequest<'update_session_title'>,
): Promise<void> {
	return invoke('update_session_title', request);
}

/** Roll back a session's durable transcript and runtime state. */
export function rollbackSession(request: TauriCommandRequest<'rollback_session'>): Promise<void> {
	return invoke('rollback_session', request);
}

/** Create a child session from one exact user message identity. */
export async function forkSessionFromMessage(
	request: TauriCommandRequest<'fork_session_from_message'>,
): Promise<string> {
	const value: unknown = await invoke('fork_session_from_message', request);
	if (typeof value !== 'string' || !isCanonicalEntityId(value, 'ses')) {
		throw new Error('Invalid fork_session_from_message response');
	}
	return value;
}

/** End the active session through the session lifecycle boundary. */
export function endSession(request: TauriCommandRequest<'end_session'>): Promise<void> {
	return invoke('end_session', request);
}

/** Interrupt the active session's current output. */
export function interruptSession(request: TauriCommandRequest<'interrupt_session'>): Promise<void> {
	return invoke('interrupt_session', request);
}

/** Continue the active session's paused or failed run. */
export function continueSession(request: TauriCommandRequest<'continue_session'>): Promise<void> {
	return invoke('continue_session', request);
}
