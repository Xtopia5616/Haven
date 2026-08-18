import { submitTranscript } from './submit.ts';

/**
 * Deliver a transcribed voice clip through `process_transcript`. The shared
 * submit helper handles the optimistic bubble, the `SessionCreated` migration
 * (from `_draft` or a stale active session id), and the failure rollback.
 *
 * @param {string} text
 * @returns {Promise<any>} the `process_transcript` result
 */
export function submitVoiceTranscript(text) {
	return submitTranscript(text, { voice: true });
}
