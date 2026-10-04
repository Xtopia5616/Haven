import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	createRecordingOverlayController,
	type RecordingOverlayController,
} from './recordingOverlayController.ts';

type RecordingCommand = 'start_recording' | 'stop_recording' | 'cancel_recording';

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function makeController(invoke = vi.fn<(command: RecordingCommand) => Promise<unknown>>()) {
	const controller = createRecordingOverlayController({
		invoke,
		now: () => 1_000,
	});
	return { controller, invoke };
}

function state(controller: RecordingOverlayController) {
	return get(controller.state);
}

describe('recording overlay controller', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('owns optimistic toolbar transitions and elapsed-time timer', async () => {
		const start = deferred<void>();
		const stop = deferred<void>();
		const invoke = vi.fn((command: RecordingCommand) =>
			command === 'start_recording' ? start.promise : stop.promise,
		);
		const { controller } = makeController(invoke);

		const starting = controller.startFromToolbar();
		expect(state(controller)).toMatchObject({ visible: true, isRecording: true, sessionId: null });
		start.resolve();
		await starting;
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		vi.advanceTimersByTime(3_000);
		expect(controller.getDuration()).toBe(3);

		const stopping = controller.stopFromToolbar();
		expect(state(controller)).toMatchObject({ visible: false, isRecording: false, sessionId: 'rec-a' });
		expect(vi.getTimerCount()).toBe(0);
		controller.onRecordingStopped({
			isRecording: false,
			sessionId: 'rec-a',
			reason: 'manual',
		});
		stop.resolve();
		await stopping;
		expect(state(controller)).toMatchObject({ visible: true, isRecording: false, sessionId: 'rec-a' });
		expect(invoke.mock.calls.map(([command]) => command)).toEqual([
			'start_recording',
			'stop_recording',
		]);
	});

	it('handles a quick start-stop click and an automatic stop transition', async () => {
		const start = deferred<void>();
		const stop = deferred<void>();
		const invoke = vi.fn((command: RecordingCommand) =>
			command === 'start_recording' ? start.promise : stop.promise,
		);
		const { controller } = makeController(invoke);

		const starting = controller.toggleFromToolbar();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-fast' });
		const stopping = controller.toggleFromToolbar();
		expect(state(controller)).toMatchObject({ visible: false, isRecording: false });
		controller.onRecordingStopped({
			isRecording: false,
			sessionId: 'rec-fast',
			reason: 'silence',
		});
		expect(state(controller)).toMatchObject({ visible: true, processing: true, reason: 'silence' });
		start.resolve();
		stop.resolve();
		await Promise.all([starting, stopping]);
		expect(invoke.mock.calls.map(([command]) => command)).toEqual([
			'start_recording',
			'stop_recording',
		]);

		controller.onTranscriptionStarted('rec-fast');
		expect(state(controller).processing).toBe(true);
		controller.onTranscriptionFinished('rec-fast');
		expect(state(controller).visible).toBe(false);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('rolls back a failed start but keeps a newer confirmed event', async () => {
		const failedStart = deferred<void>();
		const invoke = vi.fn(() => failedStart.promise);
		const { controller } = makeController(invoke);

		const starting = controller.startFromToolbar();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		failedStart.reject(new Error('start failed after a newer lifecycle event'));
		await starting;
		expect(state(controller)).toMatchObject({ visible: true, isRecording: true, sessionId: 'rec-a' });
		controller.dispose();

		const failedController = makeController(vi.fn().mockRejectedValue(new Error('busy'))).controller;
		await failedController.startFromToolbar();
		expect(state(failedController)).toMatchObject({ visible: false, isRecording: false, sessionId: null });
	});

	it('restores a matching recording when an optimistic stop fails', async () => {
		const invoke = vi.fn().mockRejectedValue(new Error('stop failed'));
		const failingController = makeController(invoke).controller;
		failingController.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });

		await expect(failingController.stopFromToolbar()).rejects.toThrow('stop failed');
		expect(state(failingController)).toMatchObject({ visible: true, isRecording: true, sessionId: 'rec-a' });
		expect(vi.getTimerCount()).toBe(1);
		failingController.dispose();
	});

	it('ignores an older stop and transcription lifecycle without stopping the new timer', () => {
		const { controller } = makeController();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		controller.onRecordingStopped({ isRecording: false, sessionId: 'rec-a', reason: 'manual' });
		controller.onTranscriptionStarted('rec-a');
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-b' });
		vi.advanceTimersByTime(2_000);

		controller.onTranscriptionStarted('rec-a');
		controller.onRecordingStopped({ isRecording: false, sessionId: 'rec-a', reason: 'cancel' });
		controller.onRecordingError({ sessionId: 'rec-a', error: 'old recording failed' });
		controller.onTranscriptionFinished('rec-a');

		expect(state(controller)).toMatchObject({ visible: true, isRecording: true, sessionId: 'rec-b' });
		expect(controller.getDuration()).toBe(2);
		vi.advanceTimersByTime(1_000);
		expect(controller.getDuration()).toBe(3);

		controller.onTranscriptionStarted('rec-b');
		expect(state(controller)).toMatchObject({ visible: true, isRecording: false, processing: true });
		controller.onTranscriptionFinished('rec-b');
		expect(state(controller)).toMatchObject({ visible: false, processing: false, sessionId: null });
		expect(vi.getTimerCount()).toBe(0);
	});

	it('applies VAD only while recording and only lets a matching stop change the overlay', () => {
		const { controller } = makeController();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		controller.onVadStatus({ state: 'speech', signal: 'speech_start' });
		expect(state(controller).vadState).toBe('speech');

		controller.onRecordingStopped({ isRecording: false, sessionId: 'rec-other', reason: 'cancel' });
		expect(state(controller)).toMatchObject({ isRecording: true, sessionId: 'rec-a' });
		controller.onRecordingStopped({ isRecording: false, sessionId: 'rec-a', reason: 'manual' });
		controller.onVadStatus({ state: 'silent', signal: 'none' });
		expect(state(controller).vadState).toBe('silent');
	});

	it('treats a repeated started event for the same capture as an idempotent confirmation', () => {
		const { controller } = makeController();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		vi.advanceTimersByTime(2_000);
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });

		expect(controller.getDuration()).toBe(2);
		expect(vi.getTimerCount()).toBe(1);
		controller.dispose();
	});

	it('hides after cancel even on command failure and disposes only its timer', async () => {
		const failedCancel = makeController(vi.fn().mockRejectedValue(new Error('cancel failed')));
		failedCancel.controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		const cancelResult = failedCancel.controller.cancel();
		await expect(cancelResult).rejects.toThrow('cancel failed');
		expect(state(failedCancel.controller)).toMatchObject({ visible: false, isRecording: false });

		const { controller, invoke } = makeController();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-b' });
		vi.advanceTimersByTime(2_000);
		controller.dispose();
		vi.advanceTimersByTime(5_000);
		expect(state(controller)).toMatchObject({ visible: true, isRecording: true, sessionId: 'rec-b' });
		expect(controller.getDuration()).toBe(2);
		expect(invoke).not.toHaveBeenCalled();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('retires a muted or cancelled overlay so late transcription events stay hidden', async () => {
		const cancel = deferred<void>();
		const { controller } = makeController(vi.fn(() => cancel.promise));
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		const cancelling = controller.cancel();
		cancel.resolve();
		await cancelling;
		expect(state(controller).sessionId).toBeNull();
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-b' });
		controller.reset('muted');
		controller.onTranscriptionStarted('rec-b');
		expect(state(controller)).toMatchObject({ visible: false, processing: false, reason: 'muted' });
	});

	it('does not let an older cancel completion clear a newer recording', async () => {
		const cancel = deferred<void>();
		const { controller } = makeController(vi.fn(() => cancel.promise));
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-a' });
		const cancelling = controller.cancel();
		controller.onRecordingStopped({ isRecording: false, sessionId: 'rec-a', reason: 'cancel' });
		controller.onRecordingStarted({ isRecording: true, sessionId: 'rec-b' });
		cancel.resolve();
		await cancelling;

		expect(state(controller)).toMatchObject({ visible: true, isRecording: true, sessionId: 'rec-b' });
		expect(vi.getTimerCount()).toBe(1);
		controller.dispose();
	});
});
