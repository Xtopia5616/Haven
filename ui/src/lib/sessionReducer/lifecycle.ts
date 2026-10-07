import { isBusyStatus } from '../sessionStatus.ts';
import { messagesOf } from './state.ts';
import type { SessionActionOf, SessionReducerState, SessionSummary } from './types.ts';

type LifecycleReducerAction = SessionActionOf<
	| 'sessions/loaded'
	| 'sessions/cleared'
	| 'session/created'
	| 'session/selected'
	| 'session/cleared'
	| 'session/status-updated'
	| 'session/run-end-notice-cleared'
	| 'session/error-reason-remembered'
	| 'session/error-reason-forgotten'
	| 'session/run-ended'
	| 'session/retained-error'
	| 'session/title-updated'
	| 'session/deleted'
>;

function cloneSession(session: SessionSummary): SessionSummary {
	return { ...session };
}
export function reduceLifecycle(
	inputState: SessionReducerState,
	action: LifecycleReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/loaded': {
			const sessions = action.sessions.map(cloneSession);
			const activeRunEndNotice =
				state.runEndNotice && state.runEndNotice.sessionId === state.activeSessionId
					? state.runEndNotice
					: null;
			if (
				activeRunEndNotice &&
				!sessions.some((session) => session.id === activeRunEndNotice.sessionId)
			) {
				const previous = state.sessions.find(
					(session) => session.id === activeRunEndNotice.sessionId,
				);
				sessions.push({
					...(previous || { id: activeRunEndNotice.sessionId }),
					status: activeRunEndNotice.status,
				});
			}
			if (action.autoSelect && !state.activeSessionId) {
				const firstActive = sessions.find(
					(session) =>
						(isBusyStatus(session.status) || session.status === 'paused') &&
						messagesOf(state, session.id).length > 0,
				);
				return { ...state, sessions, activeSessionId: firstActive?.id || null };
			}
			return { ...state, sessions };
		}
		case 'sessions/cleared': {
			return {
				...state,
				sessions: [],
				activeSessionId: null,
				runEndNotice: null,
				sessionErrorReasons: {},
			};
		}
		case 'session/created': {
			const sessions = state.sessions.some((session) => session.id === action.sessionId)
				? state.sessions.map((session) =>
						session.id === action.sessionId
							? {
									...session,
									...(action.status ? { status: action.status } : {}),
									...(action.title != null ? { title: action.title } : {}),
								}
							: session,
					)
				: [
						...state.sessions,
						{
							id: action.sessionId,
							status: action.status || 'pending',
							title: action.title || null,
						},
					];
			return action.freshStart && !action.adoptedDraft
				? { ...state, sessions }
				: {
						...state,
						sessions,
						activeSessionId: action.sessionId,
						runEndNotice: null,
					};
		}
		case 'session/selected':
			return {
				...state,
				activeSessionId: action.sessionId,
				runEndNotice:
					state.runEndNotice && state.runEndNotice.sessionId !== action.sessionId
						? null
						: state.runEndNotice,
			};
		case 'session/cleared':
			return { ...state, activeSessionId: null, runEndNotice: null };
		case 'session/status-updated': {
			const current = state.sessions.find((session) => session.id === action.sessionId);
			if (!current) return state;
			const waitingReason =
				action.waitingReason !== undefined ? action.waitingReason : current.waitingReason;
			const title = action.title != null ? action.title : current.title;
			const sessionChanged =
				current.status !== action.status ||
				current.waitingReason !== waitingReason ||
				current.title !== title;
			const runEndNoticeInvalidated =
				state.runEndNotice?.sessionId === action.sessionId &&
				action.status !== 'completed' &&
				action.status !== 'error';
			if (!sessionChanged && !runEndNoticeInvalidated) return state;
			const sessions = sessionChanged
				? state.sessions.map((session) =>
						session.id === action.sessionId
							? {
									...session,
									status: action.status,
									...(action.waitingReason !== undefined
										? { waitingReason: action.waitingReason }
										: {}),
									...(action.title != null ? { title: action.title } : {}),
								}
							: session,
					)
				: state.sessions;
			return {
				...state,
				sessions,
				...(runEndNoticeInvalidated ? { runEndNotice: null } : {}),
			};
		}
		case 'session/run-end-notice-cleared':
			return !action.sessionId || state.runEndNotice?.sessionId === action.sessionId
				? { ...state, runEndNotice: null }
				: state;
		case 'session/error-reason-remembered': {
			const reason = action.reason.trim();
			if (
				!action.sessionId ||
				!reason ||
				state.sessionErrorReasons[action.sessionId] === reason
			)
				return state;
			return {
				...state,
				sessionErrorReasons: { ...state.sessionErrorReasons, [action.sessionId]: reason },
			};
		}
		case 'session/error-reason-forgotten': {
			if (!action.sessionId || !(action.sessionId in state.sessionErrorReasons)) return state;
			const sessionErrorReasons = { ...state.sessionErrorReasons };
			delete sessionErrorReasons[action.sessionId];
			return { ...state, sessionErrorReasons };
		}
		case 'session/run-ended': {
			const current = state.sessions.find((session) => session.id === action.sessionId);
			const isActive = state.activeSessionId === action.sessionId;
			const waitingReason =
				action.waitingReason !== undefined ? action.waitingReason : current?.waitingReason;
			const title = action.title != null ? action.title : current?.title;
			const sessionChanged =
				!!current &&
				(current.status !== action.status ||
					current.waitingReason !== waitingReason ||
					current.title !== title);
			const noticeChanged =
				isActive &&
				(state.runEndNotice?.sessionId !== action.sessionId ||
					state.runEndNotice.status !== action.status ||
					state.runEndNotice.reason !== action.reason);
			if (!sessionChanged && !noticeChanged) return state;
			const sessions = sessionChanged
				? state.sessions.map((session) =>
						session.id === action.sessionId
							? {
									...session,
									status: action.status,
									...(action.waitingReason !== undefined
										? { waitingReason: action.waitingReason }
										: {}),
									...(action.title != null ? { title: action.title } : {}),
								}
							: session,
					)
				: state.sessions;
			return {
				...state,
				sessions,
				...(isActive
					? {
							runEndNotice: {
								sessionId: action.sessionId,
								status: action.status,
								reason: action.reason,
							},
						}
					: {}),
			};
		}
		case 'session/retained-error': {
			const sessions = state.sessions.some((session) => session.id === action.session.id)
				? state.sessions.map((session) =>
						session.id === action.session.id
							? { ...session, ...action.session, status: 'error' }
							: session,
					)
				: [...state.sessions, { ...action.session, status: 'error' }];
			return { ...state, sessions };
		}
		case 'session/title-updated':
			return {
				...state,
				sessions: state.sessions.map((session) =>
					session.id === action.sessionId ? { ...session, title: action.title } : session,
				),
			};
		case 'session/deleted': {
			if (!action.sessionId) return state;
			const sessionErrorReasons = { ...state.sessionErrorReasons };
			delete sessionErrorReasons[action.sessionId];
			return {
				...state,
				sessions: state.sessions.filter((session) => session.id !== action.sessionId),
				sessionErrorReasons,
				activeSessionId:
					state.activeSessionId === action.sessionId ? null : state.activeSessionId,
				runEndNotice:
					state.runEndNotice?.sessionId === action.sessionId ? null : state.runEndNotice,
			};
		}
	}
	return inputState;
}
