import { get, writable, type Readable } from 'svelte/store';
import type {
	RecordingErrorPayload,
	RecordingPayload,
	VadStatusPayload,
} from './contracts/recording.ts';
import type { TauriCommandName } from './contracts/generatedCommands.ts';
import { invoke } from './tauri.ts';

export type RecordingOverlayState = {
	visible: boolean;
	isRecording: boolean;
	processing: boolean;
	recordingId: string | null;
	startedAt: string | number | null;
	vadState: string;
};

type RecordingCommandName = Extract<
	TauriCommandName,
	'start_recording' | 'stop_recording' | 'cancel_recording'
>;

export interface RecordingOverlayControllerDependencies {
	invoke: (command: RecordingCommandName) => Promise<unknown>;
	now?: () => number;
	setInterval?: typeof globalThis.setInterval;
	clearInterval?: typeof globalThis.clearInterval;
}

export interface RecordingOverlayController {
	state: Readable<Readonly<RecordingOverlayState>>;
	duration: Readable<number>;
	snapshot: () => Readonly<RecordingOverlayState>;
	durationSeconds: () => number;
	resumeTimer: () => void;
	startFromToolbar: () => Promise<void>;
	stopFromToolbar: () => Promise<void>;
	toggleFromToolbar: () => Promise<void>;
	cancel: () => Promise<void>;
	onRecordingStarted: (event: RecordingPayload) => void;
	onRecordingStopped: (event: RecordingPayload) => void;
	onVadStatus: (event: VadStatusPayload) => void;
	onRecordingError: (event: RecordingErrorPayload) => void;
	onTranscriptionStarted: (recordingId: string) => void;
	onTranscriptionFinished: (recordingId: string) => void;
	reset: () => void;
	dispose: () => void;
}

const RECORDING_HISTORY_LIMIT = 128;

function initialState(): RecordingOverlayState {
	return {
		visible: false,
		isRecording: false,
		processing: false,
		recordingId: null,
		startedAt: null,
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
	const stoppedRecordingIds = new Set<string>();
	const terminalRecordingIds = new Set<string>();
	let durationTimer: ReturnType<typeof setInterval> | null = null;
	let lifecycleRevision = 0;

	function remember(recordingIds: Set<string>, recordingId: string | null) {
		if (!recordingId) return;
		recordingIds.delete(recordingId);
		recordingIds.add(recordingId);
		if (recordingIds.size > RECORDING_HISTORY_LIMIT) {
			const oldest = recordingIds.values().next().value;
			if (oldest !== undefined) recordingIds.delete(oldest);
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

	function reset() {
		const current = get(overlayStore);
		remember(terminalRecordingIds, current.recordingId);
		stopTimer();
		update(initialState());
	}

	async function startFromToolbar() {
		const optimisticRevision = update({
			visible: true,
			isRecording: true,
			processing: false,
			recordingId: null,
			startedAt: null,
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
				get(overlayStore).recordingId === before.recordingId
			) {
				update({ isRecording: true, visible: true });
				if (before.recordingId) startTimer(false);
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
		const recordingId = event.recordingId ?? null;
		if (
			recordingId &&
			(stoppedRecordingIds.has(recordingId) || terminalRecordingIds.has(recordingId))
		)
			return;

		const current = get(overlayStore);
		if (recordingId && current.recordingId === recordingId) {
			// A duplicate started event reconciles a missed UI start without
			// restarting the elapsed timer for the same capture.
			update({
				visible: true,
				isRecording: true,
				processing: false,
			});
			if (durationTimer === null) startTimer(false);
			return;
		}

		update({
			visible: true,
			isRecording: true,
			processing: false,
			recordingId,
			startedAt: now(),
			vadState: 'silent',
		});
		startTimer(true);
	}

	function onRecordingStopped(event: RecordingPayload) {
		const recordingId = event.recordingId ?? null;
		remember(stoppedRecordingIds, recordingId);
		if (!recordingId || get(overlayStore).recordingId !== recordingId) return;

		const reason = event.reason;
		if (reason === 'cancel') {
			reset();
			return;
		}

		const processing = reason === 'silence' || reason === 'max_duration';
		update({ isRecording: false, processing, visible: true, vadState: 'silent' });
		stopTimer();
	}

	function onVadStatus(event: VadStatusPayload) {
		if (!get(overlayStore).isRecording) return;
		// VAD events do not carry a recording ID, so they can only be
		// gated by the currently visible recording state.
		update({ vadState: event.state || 'silent' }, false);
	}

	function onRecordingError(event: RecordingErrorPayload) {
		if (get(overlayStore).recordingId === event.recordingId) reset();
	}

	function onTranscriptionStarted(recordingId: string) {
		if (terminalRecordingIds.has(recordingId) || get(overlayStore).recordingId !== recordingId)
			return;
		update({ isRecording: false, processing: true, visible: true, vadState: 'silent' });
		stopTimer();
	}

	function onTranscriptionFinished(recordingId: string) {
		remember(terminalRecordingIds, recordingId);
		if (get(overlayStore).recordingId === recordingId) reset();
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
		snapshot: () => ({ ...get(overlayStore) }),
		durationSeconds: () => get(durationStore),
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
	invoke: (command: RecordingCommandName) => invoke(command),
});
