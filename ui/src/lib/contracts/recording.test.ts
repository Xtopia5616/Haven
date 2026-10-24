import { describe, expect, it } from 'vitest';
import { mapRecordingEvent } from './recording.ts';

describe('recording IPC contract', () => {
	it('converts a recording lifecycle payload to camelCase', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 1,
			payload: {
				is_recording: false,
				recording_id: 'rec-740e8385526b3df7e1871ffaa127850a',
				reason: 'silence',
				duration_ms: 1200,
				future_field: 'ignored',
			},
		});
		expect(event).toEqual({
			event: 'recording:stopped',
			id: 1,
			payload: {
				recordingId: 'rec-740e8385526b3df7e1871ffaa127850a',
				reason: 'silence',
			},
		});
	});

	it('maps only generated VAD signal and state values and ignores additive fields', () => {
		const event = mapRecordingEvent({
			event: 'recording:vad_status',
			id: 2,
			payload: { signal: 'speech_start', state: 'speech', future_field: true },
		});
		expect(event?.payload).toEqual({ signal: 'speech_start', state: 'speech' });
		expect(
			mapRecordingEvent({
				event: 'recording:vad_status',
				id: 2,
				payload: { signal: 'future_signal', state: 'speech' },
			}),
		).toBeNull();
		expect(
			mapRecordingEvent({
				event: 'recording:vad_status',
				id: 2,
				payload: { signal: 'speech_start', state: 'future_state' },
			}),
		).toBeNull();
	});

	it('maps the transcription lifecycle fields consumed by the UI', () => {
		const started = mapRecordingEvent({
			event: 'transcription:started',
			id: 3,
			payload: {
				recording_id: 'rec-080d7df150839c2b3694f90831cc6285',
				future_field: 'ignored',
			},
		});
		const result = mapRecordingEvent({
			event: 'transcription:result',
			id: 4,
			payload: {
				recording_id: 'rec-080d7df150839c2b3694f90831cc6285',
				text: 'hello',
				duration_ms: 800,
				target_session_id: 'ses-0123456789abcdef0123456789abcdef',
				selection_version: 4,
				retry_file_id: 'file-abcdef0123456789abcdef0123456789',
				confidence: 0.85,
				future_field: 'ignored',
			},
		});

		expect(started?.payload).toEqual({ recordingId: 'rec-080d7df150839c2b3694f90831cc6285' });
		expect(result?.payload).toEqual({
			recordingId: 'rec-080d7df150839c2b3694f90831cc6285',
			text: 'hello',
			durationMs: 800,
			targetSessionId: 'ses-0123456789abcdef0123456789abcdef',
			selectionVersion: 4,
			retryFileId: 'file-abcdef0123456789abcdef0123456789',
		});
	});

	it('maps recording and transcription failures to their respective payloads', () => {
		const recordingError = mapRecordingEvent({
			event: 'recording:error',
			id: 5,
			payload: {
				recording_id: 'rec-37c676076805c964ff55cbb590a027fc',
				error: 'microphone unavailable',
				future_field: 1,
			},
		});
		const transcriptionError = mapRecordingEvent({
			event: 'transcription:error',
			id: 6,
			payload: {
				recording_id: 'rec-37c676076805c964ff55cbb590a027fc',
				error: 'transcription unavailable',
				retry_file_id: 'file-abcdef0123456789abcdef0123456789',
				future_field: 1,
			},
		});

		expect(recordingError?.payload).toEqual({
			recordingId: 'rec-37c676076805c964ff55cbb590a027fc',
			error: 'microphone unavailable',
		});
		expect(transcriptionError?.payload).toEqual({
			recordingId: 'rec-37c676076805c964ff55cbb590a027fc',
			error: 'transcription unavailable',
			retryFileId: 'file-abcdef0123456789abcdef0123456789',
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

	it('rejects malformed VAD fields and invalid error identities', () => {
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

		expect(vad).toBeNull();
		expect(error).toBeNull();
	});

	it('rejects invalid Tauri event IDs and non-record payloads', () => {
		for (const id of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1, '1']) {
			expect(
				mapRecordingEvent({
					event: 'recording:started',
					id: id as number,
					payload: { recording_id: 'rec-2a7dd1d69ba34e8fd44a2568e421951c' },
				}),
			).toBeNull();
		}
		expect(
			mapRecordingEvent({
				event: 'recording:started',
				id: 1,
				payload: null,
			}),
		).toBeNull();
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
		'ses-103ac63490bc5f6c27fd45405efecd69',
		'rec-1',
		'rec-0123456789abcdef0123456789abcdeF',
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
			payload: { session_id: 'rec-732192f5bdcf906262753a88a8baedd5' },
		});

		expect(event).toBeNull();
	});

	it('does not copy redundant recording state from the wire payload', () => {
		const event = mapRecordingEvent({
			event: 'recording:started',
			id: 11,
			payload: { is_recording: true, recording_id: 'rec-2a7dd1d69ba34e8fd44a2568e421951c' },
		});
		expect(event?.payload).toEqual({ recordingId: 'rec-2a7dd1d69ba34e8fd44a2568e421951c' });
	});

	it('requires the recording channel state and validates optional durations', () => {
		const recordingId = 'rec-2a7dd1d69ba34e8fd44a2568e421951c';
		expect(
			mapRecordingEvent({
				event: 'recording:started',
				id: 1,
				payload: { is_recording: false, recording_id: recordingId },
			}),
		).toBeNull();
		expect(
			mapRecordingEvent({
				event: 'recording:stopped',
				id: 1,
				payload: { is_recording: true, recording_id: recordingId, reason: 'manual' },
			}),
		).toBeNull();
		for (const duration_ms of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, 'long']) {
			expect(
				mapRecordingEvent({
					event: 'recording:stopped',
					id: 1,
					payload: {
						is_recording: false,
						recording_id: recordingId,
						reason: 'manual',
						duration_ms,
					},
				}),
			).toBeNull();
		}
	});

	it('rejects a stop reason outside the generated closed vocabulary', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 10,
			payload: {
				is_recording: false,
				recording_id: 'rec-732192f5bdcf906262753a88a8baedd5',
				reason: 'future_reason',
			},
		});
		expect(event).toBeNull();
	});

	it.each([undefined, null, 'future_reason'])(
		'rejects a stopped event without a valid reason: %s',
		(reason) => {
			expect(
				mapRecordingEvent({
					event: 'recording:stopped',
					id: 10,
					payload: {
						recording_id: 'rec-732192f5bdcf906262753a88a8baedd5',
						...(reason !== undefined ? { reason } : {}),
					},
				}),
			).toBeNull();
		},
	);

	it('drops recording/transcription errors without a string error field', () => {
		for (const eventName of ['recording:error', 'transcription:error'] as const) {
			expect(
				mapRecordingEvent({
					event: eventName,
					id: 8,
					payload: { recording_id: 'rec-732192f5bdcf906262753a88a8baedd5' },
				}),
			).toBeNull();
			expect(
				mapRecordingEvent({
					event: eventName,
					id: 8,
					payload: {
						recording_id: 'rec-732192f5bdcf906262753a88a8baedd5',
						error: null,
					},
				}),
			).toBeNull();
		}
	});

	it('requires string transcript text and a nonnegative integer duration', () => {
		const recordingId = 'rec-732192f5bdcf906262753a88a8baedd5';
		for (const payload of [
			{ recording_id: recordingId, duration_ms: 0, selection_version: 0 },
			{ recording_id: recordingId, text: null, duration_ms: 0, selection_version: 0 },
			{ recording_id: recordingId, text: 'hello', selection_version: 0 },
			{ recording_id: recordingId, text: 'hello', duration_ms: -1, selection_version: 0 },
			{ recording_id: recordingId, text: 'hello', duration_ms: 0.5, selection_version: 0 },
			{
				recording_id: recordingId,
				text: 'hello',
				duration_ms: Number.MAX_SAFE_INTEGER + 1,
				selection_version: 0,
			},
		]) {
			expect(mapRecordingEvent({ event: 'transcription:result', id: 9, payload })).toBeNull();
		}

		expect(
			mapRecordingEvent({
				event: 'transcription:result',
				id: 9,
				payload: {
					recording_id: recordingId,
					text: '',
					duration_ms: 0,
					selection_version: 0,
				},
			})?.payload,
		).toEqual({
			recordingId,
			text: '',
			durationMs: 0,
			targetSessionId: null,
			selectionVersion: 0,
		});
	});

	it('does not project unconsumed transcription confidence', () => {
		const event = mapRecordingEvent({
			event: 'transcription:result',
			id: 9,
			payload: {
				recording_id: 'rec-215ec2389a2af957a85060a67a563c60',
				text: 'hello',
				duration_ms: 800,
				selection_version: 1,
				confidence: 0.95,
			},
		});
		expect(event?.payload).toEqual({
			recordingId: 'rec-215ec2389a2af957a85060a67a563c60',
			text: 'hello',
			durationMs: 800,
			targetSessionId: null,
			selectionVersion: 1,
		});
		for (const confidence of [null, 'high', -0.1, 1.1]) {
			expect(
				mapRecordingEvent({
					event: 'transcription:result',
					id: 9,
					payload: {
						recording_id: 'rec-215ec2389a2af957a85060a67a563c60',
						text: 'hello',
						duration_ms: 800,
						selection_version: 1,
						confidence,
					},
				}),
			).toBeNull();
		}
	});
});
