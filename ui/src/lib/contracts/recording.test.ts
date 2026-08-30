import { describe, expect, it } from 'vitest';
import { mapRecordingEvent } from './recording.ts';

describe('recording IPC contract', () => {
	it('converts a recording lifecycle payload to camelCase', () => {
		const event = mapRecordingEvent({
			event: 'recording:stopped',
			id: 1,
			payload: { is_recording: false, session_id: 'rec-1', reason: 'silence', duration_ms: 1200 },
		});
		expect(event.payload).toEqual({
			isRecording: false,
			sessionId: 'rec-1',
			reason: 'silence',
			durationMs: 1200,
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
});
