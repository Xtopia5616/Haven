import { describe, expect, it } from 'vitest';
import { mapRecordingEvent } from './recording.ts';

describe('recording IPC contract', () => {
	it('converts a recording lifecycle payload to camelCase', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 1,
			payload: {
				is_recording: false,
				recording_id: 'rec-6b86b273ff34fce19d6b804eff5a3f57',
				reason: 'silence',
				duration_ms: 1200,
				future_field: 'ignored',
			},
		});
		expect(event).toEqual({
			event: 'recording:stopped',
			id: 1,
			payload: {
				recordingId: 'rec-6b86b273ff34fce19d6b804eff5a3f57',
				reason: 'silence',
			},
		});
	});

	it('preserves future VAD strings and ignores additive payload fields', () => {
		const event = mapRecordingEvent({
			event: 'recording:vad_status',
			id: 2,
			payload: { signal: 'future_signal', state: 'future_state', future_field: true },
		});
		expect(event?.payload).toEqual({ signal: 'future_signal', state: 'future_state' });
	});

	it('maps the transcription lifecycle fields consumed by the UI', () => {
		const started = mapRecordingEvent({
			event: 'transcription:started',
			id: 3,
			payload: {
				recording_id: 'rec-4e07408562bedb8b60ce05c1decfe3ad',
				future_field: 'ignored',
			},
		});
		const result = mapRecordingEvent({
			event: 'transcription:result',
			id: 4,
			payload: {
				recording_id: 'rec-4e07408562bedb8b60ce05c1decfe3ad',
				text: 'hello',
				duration_ms: 800,
				confidence: 0.85,
				future_field: 'ignored',
			},
		});

		expect(started?.payload).toEqual({ recordingId: 'rec-4e07408562bedb8b60ce05c1decfe3ad' });
		expect(result?.payload).toEqual({
			recordingId: 'rec-4e07408562bedb8b60ce05c1decfe3ad',
			text: 'hello',
			durationMs: 800,
		});
	});

	it('maps recording and transcription failures to their respective payloads', () => {
		const recordingError = mapRecordingEvent({
			event: 'recording:error',
			id: 5,
			payload: {
				recording_id: 'rec-ef2d127de37b942baad06145e54b0c61',
				error: 'microphone unavailable',
				future_field: 1,
			},
		});
		const transcriptionError = mapRecordingEvent({
			event: 'transcription:error',
			id: 6,
			payload: {
				recording_id: 'rec-ef2d127de37b942baad06145e54b0c61',
				error: 'transcription unavailable',
				future_field: 1,
			},
		});

		expect(recordingError?.payload).toEqual({
			recordingId: 'rec-ef2d127de37b942baad06145e54b0c61',
			error: 'microphone unavailable',
		});
		expect(transcriptionError?.payload).toEqual({
			recordingId: 'rec-ef2d127de37b942baad06145e54b0c61',
			error: 'transcription unavailable',
		});
	});

	it('drops a transcription payload without a canonical recording identity', () => {
		const event = mapRecordingEvent({
			event: 'transcription:result',
			id: 1,
			payload: { recording_id: 42, text: null, duration_ms: 'slow' },
		});
		expect(event).toBeNull();
	});

	it('retains safe defaults for malformed VAD fields and drops invalid error identities', () => {
		const vad = mapRecordingEvent({
			event: 'recording:vad_status',
			id: 7,
			payload: { signal: 42, state: null },
		});
		const error = mapRecordingEvent({
			event: 'recording:error',
			id: 8,
			payload: { recording_id: 42, error: null },
		});

		expect(vad?.payload).toEqual({ signal: 'none', state: 'silent' });
		expect(error).toBeNull();
	});

	it('drops lifecycle events without a canonical recording identity', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 6,
			payload: { is_recording: false, recording_id: 3, reason: null, duration_ms: 'unknown' },
		});
		expect(event).toBeNull();
		expect(
			mapRecordingEvent({
				event: 'recording:started',
				id: 7,
				payload: { is_recording: true },
			}),
		).toBeNull();
	});

	it.each([
		'rec-short',
		'ses-0123456789abcdef0123456789abcdef',
		'rec-01234567-89ab-cdef-0123-456789abcdef',
		'rec-0123456789ABCDEF0123456789abcdef',
	])('rejects a non-canonical recording identity: %s', (recording_id) => {
		expect(
			mapRecordingEvent({
				event: 'transcription:started',
				id: 13,
				payload: { recording_id },
			}),
		).toBeNull();
	});

	it('does not accept a conversation session field as a recording identity', () => {
		const event = mapRecordingEvent({
			event: 'recording:started',
			id: 12,
			payload: { session_id: 'rec-c49fea7425fa7f8699897a97c159c669' },
		});

		expect(event).toBeNull();
	});

	it('does not copy redundant recording state from the wire payload', () => {
		const event = mapRecordingEvent({
			event: 'recording:started',
			id: 11,
			payload: { is_recording: true, recording_id: 'rec-4fc82b26aecb47d2868c4efbe3581732' },
		});
		expect(event?.payload).toEqual({ recordingId: 'rec-4fc82b26aecb47d2868c4efbe3581732' });
	});

	it('omits a stop reason outside the generated closed vocabulary', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 10,
			payload: {
				is_recording: false,
				recording_id: 'rec-c49fea7425fa7f8699897a97c159c669',
				reason: 'future_reason',
			},
		});
		expect(event?.payload).toEqual({ recordingId: 'rec-c49fea7425fa7f8699897a97c159c669' });
	});

	it('does not project unconsumed transcription confidence', () => {
		const event = mapRecordingEvent({
			event: 'transcription:result',
			id: 9,
			payload: {
				recording_id: 'rec-19581e27de7ced00ff1ce50b2047e7a5',
				text: 'hello',
				duration_ms: 800,
				confidence: 0.95,
			},
		});
		expect(event?.payload).toEqual({
			recordingId: 'rec-19581e27de7ced00ff1ce50b2047e7a5',
			text: 'hello',
			durationMs: 800,
		});
	});
});
