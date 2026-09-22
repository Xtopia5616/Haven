import { writable } from 'svelte/store';

export type ResumeTarget = {
	sessionId: string;
	summary?: string;
	title?: string | null;
	status?: string;
	wasError?: boolean;
	errorReason?: string;
};

// Set by history before navigating to chat and consumed by +page.svelte.
export const resumeTargetStore = writable<ResumeTarget | null>(null);

// Persists an explicit "start a fresh conversation" intent across restarts.
export const NEW_ACTION_INTENT_KEY = 'haven.no_auto_restore';

/**
 * While set, event-driven paths must not auto-assign an existing session to
 * `activeSessionId`; otherwise the next message could append to old history.
 */
export const newSessionIntentStore = writable(false);
