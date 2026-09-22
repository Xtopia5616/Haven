import { get, writable } from 'svelte/store';

/**
 * User-visible error reasons for sessions that failed during this app run.
 * The backend error event is already sanitized; this UI cache lets a
 * history-opened error session show the same reason without changing its
 * persisted lifecycle state just to inspect it.
 */
export const sessionErrorReasonStore = writable<Record<string, string>>({});

export function rememberSessionError(sessionId: string, reason: string) {
	const normalized = reason.trim();
	if (!sessionId || !normalized) return;
	sessionErrorReasonStore.update((reasons) => {
		if (reasons[sessionId] === normalized) return reasons;
		return { ...reasons, [sessionId]: normalized };
	});
}

export function forgetSessionError(sessionId: string) {
	if (!sessionId) return;
	sessionErrorReasonStore.update((reasons) => {
		if (!(sessionId in reasons)) return reasons;
		const next = { ...reasons };
		delete next[sessionId];
		return next;
	});
}

export function getSessionErrorReason(sessionId: string): string {
	return get(sessionErrorReasonStore)[sessionId] || '';
}
