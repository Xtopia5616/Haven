import { appSessionReducer } from './sessionReducer.ts';

/**
 * Compatibility facade for the volatile per-session error cache owned by
 * SessionReducer. This remains renderer-only state and is not persisted or sent
 * over IPC.
 */
export function rememberSessionError(sessionId: string, reason: string) {
	appSessionReducer.dispatch({
		type: 'session/error-reason-remembered',
		sessionId,
		reason,
	});
}

export function forgetSessionError(sessionId: string) {
	appSessionReducer.dispatch({
		type: 'session/error-reason-forgotten',
		sessionId,
	});
}

export function getSessionErrorReason(sessionId: string): string {
	return appSessionReducer.getSessionErrorReason(sessionId);
}
