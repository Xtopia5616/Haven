import { writable } from 'svelte/store';

export type ReActExecutionPhase =
	'idle' | 'queued' | 'requesting' | 'generating' | 'waiting_result' | 'waiting_response';

export type ReActExecutionPhaseSnapshot = {
	sessionId: string | null;
	phase: ReActExecutionPhase;
};

// ReAct execution phase for the titlebar. Model connectivity is tracked
// separately by the shell's LLM connection probe. Keep the source session so
// selected-session consumers cannot mistake background progress for their own.
export const reactExecutionPhaseStore = writable<ReActExecutionPhaseSnapshot>({
	sessionId: null,
	phase: 'idle',
});

export function updateReactExecutionPhase(sessionId: string | null, phase: ReActExecutionPhase) {
	reactExecutionPhaseStore.set({ sessionId, phase });
}

export function reactExecutionPhaseForSession(
	snapshot: ReActExecutionPhaseSnapshot,
	sessionId: string | null,
): ReActExecutionPhase {
	return sessionId !== null && snapshot.sessionId === sessionId ? snapshot.phase : 'idle';
}
