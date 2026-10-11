/** Recording and transcription IPC contract at the frontend boundary. */

import type { TauriEvent } from './tauriEvent.ts';
import {
	RECORDING_EVENT_NAMES,
	RECORDING_STOP_REASON_DTO_VALUES,
	type RecordingStopReasonDto,
	type VadStatusEvent as GeneratedVadStatusEvent,
} from './generatedCommands.ts';
import { isNumber, isOneOf, isString } from './valueGuards.ts';
import { canonicalIdField } from './wireGuards.ts';

export type RecordingEventName = (typeof RECORDING_EVENT_NAMES)[number];

export interface RecordingPayload {
	recordingId: string;
	reason?: RecordingStopReasonDto;
}

export type VadStatusPayload = GeneratedVadStatusEvent;

function isRecordingStopReason(value: unknown): value is RecordingStopReasonDto {
	return isOneOf(value, RECORDING_STOP_REASON_DTO_VALUES);
}
export interface TranscriptionStartedPayload {
	recordingId: string;
}
export interface TranscriptionResultPayload {
	recordingId: string;
	text: string;
	durationMs: number;
}
export interface RecordingErrorPayload {
	recordingId: string;
	error: string;
}
export interface TranscriptionErrorPayload {
	recordingId: string;
	error: string;
}

export interface RecordingEventPayloadMap {
	'recording:started': RecordingPayload;
	'recording:stopped': RecordingPayload;
	'recording:vad_status': VadStatusPayload;
	'recording:error': RecordingErrorPayload;
	'transcription:started': TranscriptionStartedPayload;
	'transcription:result': TranscriptionResultPayload;
	'transcription:error': TranscriptionErrorPayload;
}

/** Convert the stable snake_case Rust payload to the route-facing DTO. */
export function mapRecordingEvent<K extends RecordingEventName>(
	event: TauriEvent<Record<string, unknown>> & { event: K },
): TauriEvent<RecordingEventPayloadMap[K]> | null {
	const payload = event.payload;
	switch (event.event) {
		case 'recording:started':
		case 'recording:stopped': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			if (recordingId === null) return null;
			return {
				...event,
				payload: {
					recordingId,
					...(isRecordingStopReason(payload.reason) ? { reason: payload.reason } : {}),
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
		case 'recording:vad_status':
			return {
				...event,
				payload: {
					signal: isString(payload.signal) ? payload.signal : 'none',
					state: isString(payload.state) ? payload.state : 'silent',
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		case 'recording:error':
		case 'transcription:error': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			if (recordingId === null) return null;
			return {
				...event,
				payload: {
					recordingId,
					error: isString(payload.error) ? payload.error : '',
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
		case 'transcription:started': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			if (recordingId === null) return null;
			return {
				...event,
				payload: { recordingId },
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
		case 'transcription:result': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			if (recordingId === null) return null;
			return {
				...event,
				payload: {
					recordingId,
					text: isString(payload.text) ? payload.text : '',
					durationMs: isNumber(payload.duration_ms) ? payload.duration_ms : 0,
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
	}
}
