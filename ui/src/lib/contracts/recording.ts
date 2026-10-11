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

export interface RecordingStartedPayload {
	recordingId: string;
	reason?: RecordingStopReasonDto;
}

export interface RecordingStoppedPayload {
	recordingId: string;
	reason: RecordingStopReasonDto;
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
	'recording:started': RecordingStartedPayload;
	'recording:stopped': RecordingStoppedPayload;
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
		case 'recording:started': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			if (recordingId === null) return null;
			const reason = payload.reason;
			if (reason !== undefined && reason !== null && !isRecordingStopReason(reason))
				return null;
			return {
				...event,
				payload: {
					recordingId,
					...(isRecordingStopReason(reason) ? { reason } : {}),
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
		case 'recording:stopped': {
			const recordingId = canonicalIdField(payload, 'recording_id', 'rec');
			const reason = payload.reason;
			if (recordingId === null || !isRecordingStopReason(reason)) return null;
			return {
				...event,
				payload: { recordingId, reason },
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
			if (recordingId === null || !isString(payload.error)) return null;
			return {
				...event,
				payload: {
					recordingId,
					error: payload.error,
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
			const durationMs = payload.duration_ms;
			if (
				recordingId === null ||
				!isString(payload.text) ||
				!isNumber(durationMs) ||
				!Number.isSafeInteger(durationMs) ||
				durationMs < 0
			)
				return null;
			return {
				...event,
				payload: {
					recordingId,
					text: payload.text,
					durationMs,
				},
			} as unknown as TauriEvent<RecordingEventPayloadMap[K]>;
		}
	}
}
