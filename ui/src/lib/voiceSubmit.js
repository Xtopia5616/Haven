import { submitTranscript } from './submit.js';

/**
 * Deliver a transcribed voice clip through `process_transcript`. The shared
 * submit helper handles the optimistic bubble, the `TaskCreated` migration
 * (from `_draft` or a stale active task id), and the failure rollback.
 *
 * @param {string} text
 * @returns {Promise<any>} the `process_transcript` result
 */
export function submitVoiceTranscript(text) {
	return submitTranscript(text, { voice: true });
}
