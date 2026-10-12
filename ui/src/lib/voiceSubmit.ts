import { submitTranscript } from './submit.ts';
import { appSessionReducer } from './sessionReducer.ts';
import type { ProcessResultView } from './contracts/processResult.ts';

/**
 * Deliver a transcribed voice clip through `process_transcript`. The shared
 * submit helper handles the optimistic bubble, the `SessionCreated` migration
 * (from `_draft` or a stale active session id), and the failure rollback.
 *
 * @param {string} text
 * @returns the validated camelCase process result view
 */
export function submitVoiceTranscript(text: string, recordingId?: string): Promise<ProcessResultView> {
	return submitTranscript(text, { voice: true, recordingId, reducer: appSessionReducer });
}
