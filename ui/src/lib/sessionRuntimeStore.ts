import { writable } from 'svelte/store';

export type ReactExecutionPhase =
	'idle' | 'queued' | 'requesting' | 'generating' | 'waiting_result' | 'waiting_response';

export type ReactExecutionPhaseSnapshot = {
	sessionId: string | null;
	phase: ReactExecutionPhase;
};

// ReAct execution phase for the titlebar. Model connectivity is tracked
// separately by the shell's LLM connection probe. Keep the source session so
// selected-session consumers cannot mistake background progress for their own.
export const reactExecutionPhaseStore = writable<ReactExecutionPhaseSnapshot>({
	sessionId: null,
	phase: 'idle',
});

// Presentation status for the selected session, consumed by the shell.
export const activeSessionStatusLabelStore = writable('空闲');

export function updateReactExecutionPhase(sessionId: string | null, phase: ReactExecutionPhase) {
	reactExecutionPhaseStore.set({ sessionId, phase });
}

export function reactExecutionPhaseForSession(
	snapshot: ReactExecutionPhaseSnapshot,
	sessionId: string | null,
): ReactExecutionPhase {
	return sessionId !== null && snapshot.sessionId === sessionId ? snapshot.phase : 'idle';
}
