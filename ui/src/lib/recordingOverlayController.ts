import { get, writable, type Readable } from 'svelte/store';
import type {
	RecordingErrorPayload,
	RecordingPayload,
	VadStatusPayload,
} from './contracts/recording.ts';
import { invoke } from './tauri.ts';

export type RecordingOverlayState = {
	visible: boolean;
	isRecording: boolean;
	processing: boolean;
	sessionId: string | null;
	startedAt: string | number | null;
	reason: string | null;
	vadState: string;
};

type RecordingCommand = 'start_recording' | 'stop_recording' | 'cancel_recording';

export interface RecordingOverlayControllerDependencies {
	invoke: (command: RecordingCommand) => Promise<unknown>;
	now?: () => number;
	setInterval?: typeof globalThis.setInterval;
	clearInterval?: typeof globalThis.clearInterval;
}

export interface RecordingOverlayController {
	state: Readable<Readonly<RecordingOverlayState>>;
	duration: Readable<number>;
	getState: () => Readonly<RecordingOverlayState>;
	getDuration: () => number;
	resumeTimer: () => void;
	startFromToolbar: () => Promise<void>;
	stopFromToolbar: () => Promise<void>;
	toggleFromToolbar: () => Promise<void>;
	cancel: () => Promise<void>;
	onRecordingStarted: (event: RecordingPayload) => void;
	onRecordingStopped: (event: RecordingPayload) => void;
	onVadStatus: (event: VadStatusPayload) => void;
	onRecordingError: (event: RecordingErrorPayload) => void;
	onTranscriptionStarted: (sessionId: string) => void;
	onTranscriptionFinished: (sessionId: string) => void;
	reset: (reason?: string | null) => void;
	dispose: () => void;
}

const SESSION_HISTORY_LIMIT = 128;

function initialState(): RecordingOverlayState {
	return {
		visible: false,
		isRecording: false,
		processing: false,
		sessionId: null,
		startedAt: null,
		reason: null,
		vadState: 'silent',
	};
}

/** Own the shared recording overlay state, timer, and toolbar lifecycle calls. */
export function createRecordingOverlayController(
	dependencies: RecordingOverlayControllerDependencies,
): RecordingOverlayController {
	const overlayStore = writable(initialState());
	const durationStore = writable(0);
	const state: Readable<Readonly<RecordingOverlayState>> = {
		subscribe: (run) => overlayStore.subscribe((current) => run({ ...current })),
	};
	const duration: Readable<number> = durationStore;
	const now = dependencies.now ?? Date.now;
	const setTimer = dependencies.setInterval ?? globalThis.setInterval;
	const clearTimer = dependencies.clearInterval ?? globalThis.clearInterval;
	const stoppedSessions = new Set<string>();
	const terminalSessions = new Set<string>();
	let durationTimer: ReturnType<typeof setInterval> | null = null;
	let lifecycleRevision = 0;

	function remember(sessions: Set<string>, sessionId: string | null) {
		if (!sessionId) return;
		sessions.delete(sessionId);
		sessions.add(sessionId);
		if (sessions.size > SESSION_HISTORY_LIMIT) {
			const oldest = sessions.values().next().value;
			if (oldest !== undefined) sessions.delete(oldest);
		}
	}

	function update(patch: Partial<RecordingOverlayState>, lifecycleChange = true): number {
		if (lifecycleChange) lifecycleRevision += 1;
		overlayStore.update((current) => ({ ...current, ...patch }));
		return lifecycleRevision;
	}

	function stopTimer() {
		if (durationTimer === null) return;
		clearTimer(durationTimer);
		durationTimer = null;
	}

	function startTimer(resetDuration: boolean) {
		stopTimer();
		if (resetDuration) durationStore.set(0);
		durationTimer = setTimer(() => {
			durationStore.update((value) => value + 1);
		}, 1000);
	}

	function reset(reason: string | null = null) {
		const current = get(overlayStore);
		remember(terminalSessions, current.sessionId);
		stopTimer();
		update({ ...initialState(), reason });
	}

	async function startFromToolbar() {
		const optimisticRevision = update({
			visible: true,
			isRecording: true,
			processing: false,
			sessionId: null,
			startedAt: null,
			reason: null,
			vadState: 'silent',
		});
		try {
			await dependencies.invoke('start_recording');
		} catch {
			// Rust emits the user-facing recording:error event. The command
			// rejection only rolls back this optimistic state if no newer event
			// or recording intent has superseded it.
			if (lifecycleRevision === optimisticRevision) reset();
		}
	}

	async function stopFromToolbar() {
		const before = get(overlayStore);
		if (!before.isRecording) return startFromToolbar();

		const optimisticRevision = update({ isRecording: false, visible: false });
		stopTimer();
		try {
			await dependencies.invoke('stop_recording');
		} catch (error) {
			if (
				lifecycleRevision === optimisticRevision &&
				get(overlayStore).sessionId === before.sessionId
			) {
				update({ isRecording: true, visible: true });
				if (before.sessionId) startTimer(false);
			}
			throw error;
		}
	}

	async function toggleFromToolbar() {
		if (get(overlayStore).isRecording) {
			await stopFromToolbar();
		} else {
			await startFromToolbar();
		}
	}

	async function cancel() {
		const revisionBeforeCancel = lifecycleRevision;
		try {
			await dependencies.invoke('cancel_recording');
		} finally {
			// A new recording intent/event that arrived during the command owns
			// the overlay now. Do not let this older cancel completion clear it.
			if (lifecycleRevision === revisionBeforeCancel) reset();
		}
	}

	function onRecordingStarted(event: RecordingPayload) {
		const sessionId = event.sessionId ?? null;
		if (sessionId && (stoppedSessions.has(sessionId) || terminalSessions.has(sessionId))) return;

		const current = get(overlayStore);
		if (sessionId && current.sessionId === sessionId) {
			// A duplicate started event reconciles a missed UI start without
			// restarting the elapsed timer for the same capture.
			update({
				visible: true,
				isRecording: true,
				processing: false,
				reason: null,
			});
			if (durationTimer === null) startTimer(false);
			return;
		}

		update({
			visible: true,
			isRecording: true,
			processing: false,
			sessionId,
			startedAt: now(),
			reason: null,
			vadState: 'silent',
		});
		startTimer(true);
	}

	function onRecordingStopped(event: RecordingPayload) {
		const sessionId = event.sessionId ?? null;
		remember(stoppedSessions, sessionId);
		if (!sessionId || get(overlayStore).sessionId !== sessionId) return;

		const reason = event.reason ?? null;
		if (reason === 'cancel') {
			reset();
			return;
		}

		const processing = reason === 'silence' || reason === 'max_duration';
		update({ isRecording: false, processing, visible: true, reason, vadState: 'silent' });
		stopTimer();
	}

	function onVadStatus(event: VadStatusPayload) {
		if (!get(overlayStore).isRecording) return;
		// VAD events do not carry a recording session id, so they can only be
		// gated by the currently visible recording state.
		update({ vadState: event.state || 'silent' }, false);
	}

	function onRecordingError(event: RecordingErrorPayload) {
		if (get(overlayStore).sessionId === event.sessionId) reset();
	}

	function onTranscriptionStarted(sessionId: string) {
		if (terminalSessions.has(sessionId) || get(overlayStore).sessionId !== sessionId) return;
		update({ isRecording: false, processing: true, visible: true, vadState: 'silent' });
		stopTimer();
	}

	function onTranscriptionFinished(sessionId: string) {
		remember(terminalSessions, sessionId);
		if (get(overlayStore).sessionId === sessionId) reset();
	}

	function resumeTimer() {
		if (get(overlayStore).isRecording && durationTimer === null) startTimer(false);
	}

	function dispose() {
		// Layout teardown owns only this controller's interval. Backend capture
		// and the global Tauri listeners have separate owners and lifetimes.
		stopTimer();
	}

	return {
		state,
		duration,
		getState: () => ({ ...get(overlayStore) }),
		getDuration: () => get(durationStore),
		resumeTimer,
		startFromToolbar,
		stopFromToolbar,
		toggleFromToolbar,
		cancel,
		onRecordingStarted,
		onRecordingStopped,
		onVadStatus,
		onRecordingError,
		onTranscriptionStarted,
		onTranscriptionFinished,
		reset,
		dispose,
	};
}

export const recordingOverlayController = createRecordingOverlayController({
	invoke: (command: RecordingCommand) => invoke(command),
});
