import { isBusyStatus } from '../sessionStatus.ts';
import { messagesOf } from './state.ts';
import type { SessionActionOf, SessionReducerState, SessionSummary } from './types.ts';

type Action = SessionActionOf<
	| 'sessions/loaded'
	| 'sessions/cleared'
	| 'session/created'
	| 'session/selected'
	| 'session/cleared'
	| 'session/status-updated'
	| 'session/error-shown'
	| 'session/error-cleared'
	| 'session/error-reason-remembered'
	| 'session/error-reason-forgotten'
	| 'session/termination-shown'
	| 'session/retained-error'
	| 'session/title-updated'
	| 'session/deleted'
>;

function cloneSession(session: SessionSummary): SessionSummary {
	return { ...session };
}
export function reduceLifecycle(
	inputState: SessionReducerState,
	action: Action,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/loaded': {
			const sessions = action.sessions.map(cloneSession);
			const activeError =
				state.error && state.error.sessionId === state.activeSessionId
					? state.sessions.find((session) => session.id === state.activeSessionId)
					: null;
			const activeTermination =
				state.termination && state.termination.sessionId === state.activeSessionId
					? state.termination
					: null;
			if (activeError && !sessions.some((session) => session.id === activeError.id)) {
				sessions.push({ ...activeError, status: 'error' });
			}
			if (
				activeTermination &&
				!sessions.some((session) => session.id === activeTermination.sessionId)
			) {
				const previous = state.sessions.find(
					(session) => session.id === activeTermination.sessionId,
				);
				sessions.push({
					...(previous || { id: activeTermination.sessionId }),
					status: activeTermination.status,
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
				error: null,
				termination: null,
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
						error: null,
						termination: null,
					};
		}
		case 'session/selected':
			return {
				...state,
				activeSessionId: action.sessionId,
				error:
					state.error && state.error.sessionId !== action.sessionId ? null : state.error,
				termination:
					state.termination && state.termination.sessionId !== action.sessionId
						? null
						: state.termination,
			};
		case 'session/cleared':
			return { ...state, activeSessionId: null, error: null, termination: null };
		case 'session/status-updated': {
			const sessions = state.sessions.map((session) =>
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
			);
			const terminalStateChanged =
				state.termination?.sessionId === action.sessionId &&
				action.status !== 'completed' &&
				action.status !== 'error';
			return {
				...state,
				sessions,
				...(state.error?.sessionId === action.sessionId && isBusyStatus(action.status)
					? { error: null }
					: {}),
				...(terminalStateChanged ? { termination: null } : {}),
			};
		}
		case 'session/error-shown': {
			const sessions = state.sessions.map((session) =>
				session.id === action.sessionId ? { ...session, status: 'error' } : session,
			);
			return state.activeSessionId === action.sessionId
				? {
						...state,
						sessions,
						error: { sessionId: action.sessionId, reason: action.reason },
						termination: {
							sessionId: action.sessionId,
							status: 'error',
							reason: action.reason,
						},
					}
				: { ...state, sessions };
		}
		case 'session/error-cleared':
			return !action.sessionId || state.error?.sessionId === action.sessionId
				? {
						...state,
						error: null,
						termination:
							!action.sessionId || state.termination?.sessionId === action.sessionId
								? null
								: state.termination,
					}
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
		case 'session/termination-shown': {
			const alreadyShown =
				state.activeSessionId === action.sessionId &&
				state.termination?.sessionId === action.sessionId &&
				state.termination.status === action.status &&
				state.termination.reason === action.reason;
			if (alreadyShown) return state;
			const sessions = state.sessions.map((session) =>
				session.id === action.sessionId ? { ...session, status: action.status } : session,
			);
			if (state.activeSessionId !== action.sessionId) return { ...state, sessions };
			return {
				...state,
				sessions,
				termination: {
					sessionId: action.sessionId,
					status: action.status,
					reason: action.reason,
				},
				error:
					action.status === 'error'
						? { sessionId: action.sessionId, reason: action.reason }
						: null,
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
			return {
				...state,
				sessions: state.sessions.filter((session) => session.id !== action.sessionId),
				activeSessionId:
					state.activeSessionId === action.sessionId ? null : state.activeSessionId,
				error: state.error?.sessionId === action.sessionId ? null : state.error,
				termination:
					state.termination?.sessionId === action.sessionId ? null : state.termination,
			};
		}
	}
	return inputState;
}
