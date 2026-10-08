import { describe, expect, it } from 'vitest';
import { mapRecordingEvent } from './recording.ts';

describe('recording IPC contract', () => {
	it('converts a recording lifecycle payload to camelCase', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 1,
			payload: {
				is_recording: false,
				session_id: 'rec-1',
				reason: 'silence',
				duration_ms: 1200,
				future_field: 'ignored',
			},
		});
		expect(event).toEqual({
			event: 'recording:stopped',
			id: 1,
			payload: {
				isRecording: false,
				sessionId: 'rec-1',
				reason: 'silence',
				durationMs: 1200,
			},
		});
	});

	it('preserves future VAD strings and ignores additive payload fields', () => {
		const event = mapRecordingEvent({
			event: 'recording:vad_status',
			id: 2,
			payload: { signal: 'future_signal', state: 'future_state', future_field: true },
		});
		expect(event.payload).toEqual({ signal: 'future_signal', state: 'future_state' });
	});

	it('maps the transcription lifecycle without changing optional confidence', () => {
		const started = mapRecordingEvent({
			event: 'transcription:started',
			id: 3,
			payload: { session_id: 'rec-3', future_field: 'ignored' },
		});
		const result = mapRecordingEvent({
			event: 'transcription:result',
			id: 4,
			payload: {
				session_id: 'rec-3',
				text: 'hello',
				duration_ms: 800,
				confidence: 0.85,
				future_field: 'ignored',
			},
		});

		expect(started.payload).toEqual({ sessionId: 'rec-3' });
		expect(result.payload).toEqual({
			sessionId: 'rec-3',
			text: 'hello',
			durationMs: 800,
			confidence: 0.85,
		});
	});

	it('maps recording and transcription failures to their respective payloads', () => {
		const recordingError = mapRecordingEvent({
			event: 'recording:error',
			id: 5,
			payload: {
				session_id: 'rec-5',
				error: 'microphone unavailable',
				future_field: 1,
			},
		});
		const transcriptionError = mapRecordingEvent({
			event: 'transcription:error',
			id: 6,
			payload: {
				session_id: 'rec-5',
				error: 'transcription unavailable',
				future_field: 1,
			},
		});

		expect(recordingError.payload).toEqual({
			sessionId: 'rec-5',
			error: 'microphone unavailable',
		});
		expect(transcriptionError.payload).toEqual({
			sessionId: 'rec-5',
			error: 'transcription unavailable',
		});
	});

	it('uses safe values for a malformed transcription payload', () => {
		const event = mapRecordingEvent({
			event: 'transcription:result',
			id: 1,
			payload: { session_id: 42, text: null, duration_ms: 'slow' },
		});
		expect(event.payload).toEqual({ sessionId: '', text: '', durationMs: 0 });
	});

	it('retains safe defaults for malformed VAD and error fields', () => {
		const vad = mapRecordingEvent({
			event: 'recording:vad_status',
			id: 7,
			payload: { signal: 42, state: null },
		});
		const error = mapRecordingEvent({
			event: 'recording:error',
			id: 8,
			payload: { session_id: 42, error: null },
		});

		expect(vad.payload).toEqual({ signal: 'none', state: 'silent' });
		expect(error.payload).toEqual({ sessionId: '', error: '' });
	});

	it('omits malformed optional recording fields', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 6,
			payload: { is_recording: false, session_id: 3, reason: null, duration_ms: 'unknown' },
		});
		expect(event.payload).toEqual({ isRecording: false });
	});

	it('omits malformed optional transcription confidence', () => {
		const event = mapRecordingEvent({
			event: 'transcription:result',
			id: 9,
			payload: {
				session_id: 'rec-9',
				text: 'hello',
				duration_ms: 800,
				confidence: 'high',
			},
		});
		expect(event.payload).toEqual({ sessionId: 'rec-9', text: 'hello', durationMs: 800 });
	});
});
