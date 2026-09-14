import { isBusyStatus } from './sessionStatus.ts';

/** The session summary fields used by the chat shell. */
export interface SessionSummary {
	id: string;
	status: string;
	[key: string]: unknown;
}

export interface SessionError {
	sessionId: string;
	reason: string;
}

export interface SessionReducerState {
	sessions: SessionSummary[];
	activeSessionId: string | null;
	error: SessionError | null;
}

export type SessionAction =
	| { type: 'sessions/loaded'; sessions: SessionSummary[] }
	| {
			type: 'session/created';
			sessionId: string;
			freshStart: boolean;
			adoptedDraft: boolean;
	  }
	| { type: 'session/selected'; sessionId: string | null }
	| { type: 'session/cleared' }
	| { type: 'session/status-updated'; sessionId: string; status: string }
	| { type: 'session/error-shown'; sessionId: string; reason: string }
	| { type: 'session/error-cleared'; sessionId?: string | null }
	| { type: 'session/retained-error'; session: SessionSummary }
	| { type: 'session/title-updated'; sessionId: string; title: string };

export const initialSessionState: SessionReducerState = {
	sessions: [],
	activeSessionId: null,
	error: null,
};

function cloneSession(session: SessionSummary): SessionSummary {
	return { ...session };
}

/**
 * Pure session state transition function.
 *
 * Message projection, stream cleanup and IPC are deliberately outside this
 * reducer. Those effects can be ordered by the event adapter without making
 * the UI's selection/error policy implicit in callback wiring.
 */
export function reduceSession(
	state: SessionReducerState,
	action: SessionAction,
): SessionReducerState {
	switch (action.type) {
		case 'sessions/loaded': {
			const sessions = action.sessions.map(cloneSession);
			const activeError =
				state.error && state.error.sessionId === state.activeSessionId
					? state.sessions.find((session) => session.id === state.activeSessionId)
					: null;
			if (activeError && !sessions.some((session) => session.id === activeError.id)) {
				sessions.push({ ...activeError, status: 'error' });
			}
			return { ...state, sessions };
		}

		case 'session/created':
			return action.freshStart && !action.adoptedDraft
				? state
				: { ...state, activeSessionId: action.sessionId, error: null };

		case 'session/selected':
			return {
				...state,
				activeSessionId: action.sessionId,
				error:
					state.error && state.error.sessionId !== action.sessionId ? null : state.error,
			};

		case 'session/cleared':
			return { ...state, activeSessionId: null, error: null };

		case 'session/status-updated':
			return state.error?.sessionId === action.sessionId && isBusyStatus(action.status)
				? { ...state, error: null }
				: state;

		case 'session/error-shown':
			return state.activeSessionId === action.sessionId
				? { ...state, error: { sessionId: action.sessionId, reason: action.reason } }
				: state;

		case 'session/error-cleared':
			return !action.sessionId || state.error?.sessionId === action.sessionId
				? { ...state, error: null }
				: state;

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
	}
}

type SessionStateListener = (state: SessionReducerState) => void;

/** Small observable wrapper used by the Svelte route and unit tests. */
export class SessionReducer {
	private state: SessionReducerState;
	private readonly listeners = new Set<SessionStateListener>();

	constructor(initialState: SessionReducerState = initialSessionState) {
		this.state = initialState;
	}

	getState(): SessionReducerState {
		return this.state;
	}

	dispatch(action: SessionAction): SessionReducerState {
		this.state = reduceSession(this.state, action);
		for (const listener of this.listeners) listener(this.state);
		return this.state;
	}

	subscribe(listener: SessionStateListener): () => void {
		this.listeners.add(listener);
		listener(this.state);
		return () => this.listeners.delete(listener);
	}
}
