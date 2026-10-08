import { writable } from 'svelte/store';
import type { SessionRunEndStatus } from './sessionReducer.ts';

export type SessionResumeTarget = {
	sessionId: string;
	summary?: string;
	title?: string | null;
	runEndStatus?: SessionRunEndStatus;
	runEndReason?: string;
	wasError?: boolean;
	errorReason?: string;
};

// Set by history before navigating to chat and consumed by +page.svelte.
export const sessionResumeTargetStore = writable<SessionResumeTarget | null>(null);

// Persists an explicit "start a new session" intent across restarts.
export const NEW_SESSION_INTENT_STORAGE_KEY = 'haven.no_auto_restore';

/**
 * While set, event-driven paths must not auto-assign an existing session to
 * `activeSessionId`; otherwise the next message could append to old history.
 */
export const newSessionIntentStore = writable(false);
