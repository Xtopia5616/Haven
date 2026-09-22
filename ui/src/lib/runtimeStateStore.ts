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

// Shared recording UI state consumed by the layout overlay and input router.
export const recordingOverlay = writable<RecordingOverlayState>({
	visible: false,
	isRecording: false,
	processing: false,
	sessionId: null,
	startedAt: null,
	reason: null,
	vadState: 'silent',
});

export type ModelState = 'ready' | 'waiting' | 'streaming' | 'tool' | 'stalled';

// Runtime status for the titlebar model chip.
export const modelStateStore = writable<ModelState>('ready');

// Presentation status for the selected conversation, consumed by the shell.
export const activeConversationStatusStore = writable('就绪');

let modelStateTimer: ReturnType<typeof setTimeout> | null = null;

export function updateModelState(state: ModelState, opts: { idleTimeoutMs?: number } = {}) {
	const { idleTimeoutMs } = opts;
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
	modelStateStore.set(state);
	if (state === 'waiting') {
		modelStateTimer = setTimeout(() => {
			modelStateTimer = null;
			modelStateStore.update((current) => (current === 'waiting' ? 'ready' : current));
		}, idleTimeoutMs ?? 5000);
	} else if (state === 'streaming') {
		modelStateTimer = setTimeout(() => {
			modelStateTimer = null;
			modelStateStore.update((current) => (current === 'streaming' ? 'ready' : current));
		}, idleTimeoutMs ?? 2000);
	}
}

export function clearModelStateTimer() {
	if (modelStateTimer) clearTimeout(modelStateTimer);
	modelStateTimer = null;
}
