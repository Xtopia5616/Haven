import { isBusyStatus, isPausedStatus } from './sessionStatus.ts';
import type { SessionHistoryRow } from './contracts/sessionHistory.ts';
import type { SessionSummary } from './sessionReducer/types.ts';

/** Combine live sessions with recent persisted history for the compact switcher. */
export function buildSessionSwitcherOptions(
	sessions: SessionSummary[],
	history: SessionHistoryRow[],
): SessionSummary[] {
	const liveSessions = sessions.filter(
		(session) => isBusyStatus(session.status) || isPausedStatus(session.status),
	);
	const liveIds = new Set(liveSessions.map((session) => session.id));
	return [
		...liveSessions,
		...history
			.filter((session) => !liveIds.has(session.id))
			.map((session) => ({
				...session,
				title: session.title || session.input_text,
			})),
	];
}
