import { submitTranscript } from './submit.ts';
import { appSessionReducer } from './sessionReducer.ts';
import type { ProcessResult } from './contracts/generatedCommands.ts';

/**
 * Deliver a transcribed voice clip through `process_transcript`. The shared
 * submit helper handles the optimistic bubble, the `SessionCreated` migration
 * (from `_draft` or a stale active session id), and the failure rollback.
 *
 * @param {string} text
 * @returns the generated `ProcessResult` contract
 */
export function submitVoiceTranscript(
	text: string,
	recordingSessionId?: string,
): Promise<ProcessResult> {
	return submitTranscript(text, { voice: true, recordingSessionId, reducer: appSessionReducer });
}
