import { writable } from 'svelte/store';

export type RecordingOverlayState = {
	visible: boolean;
	isRecording: boolean;
	processing: boolean;
	sessionId: string | null;
	startedAt: string | number | null;
	reason: string | null;
	vadState: string;
};

export type ReactExecutionPhase =
	'idle' | 'queued' | 'requesting' | 'generating' | 'waiting_result' | 'waiting_response';

// ReAct execution phase for the titlebar. Model connectivity is tracked
// separately by the shell's LLM connection probe.
export const reactExecutionPhaseStore = writable<ReactExecutionPhase>('idle');

// Presentation status for the selected conversation, consumed by the shell.
export const activeConversationStatusStore = writable('空闲');

export function updateReactExecutionPhase(phase: ReactExecutionPhase) {
	reactExecutionPhaseStore.set(phase);
}
