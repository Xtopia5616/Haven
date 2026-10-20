import { isErrorStatus } from './sessionStatus.ts';
import type { SessionHistoryRow } from './contracts/sessionHistory.ts';
import type { SessionSummary } from './sessionReducer/types.ts';

/** Show open runtime sessions plus resumable paused/error history in the compact switcher. */
export function buildSessionSwitcherOptions(
	sessions: SessionSummary[],
	history: SessionHistoryRow[],
): SessionSummary[] {
	const openSessions = sessions.filter((session) => session.status !== 'completed');
	// Current reducer state wins over a potentially stale history snapshot. In
	// particular, a completed lifecycle event must hide the matching history row
	// immediately, before the next history refresh.
	const knownSessionIds = new Set(sessions.map((session) => session.sessionId));
	return [
		...openSessions,
		...history
			.filter(
				(session) =>
					!knownSessionIds.has(session.sessionId) &&
					(session.status === 'paused' || isErrorStatus(session.status)),
			)
			.map((session) => ({
				sessionId: session.sessionId,
				status: session.status,
				title: session.title || session.input_text,
				inputText: session.input_text,
			})),
	];
}
