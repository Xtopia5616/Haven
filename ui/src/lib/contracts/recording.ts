/** Recording and transcription IPC contract at the frontend boundary. */

import type { TauriEvent } from './session.ts';

export const RECORDING_EVENT_NAMES = [
	'recording:started',
	'recording:stopped',
	'recording:vad_status',
	'recording:error',
	'transcription:started',
	'transcription:result',
	'transcription:error',
] as const;

export type RecordingEventName = (typeof RECORDING_EVENT_NAMES)[number];

export interface RecordingPayload {
	isRecording: boolean;
	sessionId?: string;
	reason?: string;
	durationMs?: number;
}

export interface VadStatusPayload { signal: string; state: string; }
export interface TranscriptionStartedPayload { sessionId: string; }
export interface TranscriptionResultPayload {
	sessionId: string;
	text: string;
	durationMs: number;
	confidence?: number;
}
export interface RecordingErrorPayload { sessionId: string; error: string; }

export interface RecordingEventPayloadMap {
	'recording:started': RecordingPayload;
	'recording:stopped': RecordingPayload;
	'recording:vad_status': VadStatusPayload;
	'recording:error': RecordingErrorPayload;
	'transcription:started': TranscriptionStartedPayload;
	'transcription:result': TranscriptionResultPayload;
	'transcription:error': RecordingErrorPayload;
}

/** Convert the stable snake_case Rust payload to the route-facing DTO. */
export function mapRecordingEvent<K extends RecordingEventName>(
	event: TauriEvent<Record<string, unknown>> & { event: K },
): TauriEvent<RecordingEventPayloadMap[K]> {
	const payload = event.payload;
	switch (event.event) {
		case 'recording:started':
		case 'recording:stopped':
			return { ...event, payload: {
				isRecording: Boolean(payload.is_recording),
				...(typeof payload.session_id === 'string' ? { sessionId: payload.session_id } : {}),
				...(typeof payload.reason === 'string' ? { reason: payload.reason } : {}),
				...(typeof payload.duration_ms === 'number' ? { durationMs: payload.duration_ms } : {}),
			} } as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		case 'recording:vad_status':
			return { ...event, payload: {
				signal: typeof payload.signal === 'string' ? payload.signal : 'none',
				state: typeof payload.state === 'string' ? payload.state : 'silent',
			} } as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		case 'recording:error':
		case 'transcription:error':
			return { ...event, payload: {
				sessionId: typeof payload.session_id === 'string' ? payload.session_id : '',
				error: typeof payload.error === 'string' ? payload.error : '',
			} } as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		case 'transcription:started':
			return { ...event, payload: {
				sessionId: typeof payload.session_id === 'string' ? payload.session_id : '',
			} } as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		case 'transcription:result':
			return { ...event, payload: {
				sessionId: typeof payload.session_id === 'string' ? payload.session_id : '',
				text: typeof payload.text === 'string' ? payload.text : '',
				durationMs: typeof payload.duration_ms === 'number' ? payload.duration_ms : 0,
				...(typeof payload.confidence === 'number' ? { confidence: payload.confidence } : {}),
			} } as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
	}
}
