import { submitTranscript } from './submit.ts';
import { appSessionReducer } from './sessionReducer.ts';
import type { TranscriptSubmissionResultView } from './contracts/transcriptSubmissionResult.ts';

/**
 * Deliver a transcribed voice clip through `submit_transcript`. The shared
 * submit helper handles the optimistic bubble, the `SessionCreated` migration
 * (from `_draft` or a stale active session id), and the failure rollback.
 *
 * @param {string} text
 * @returns the validated camelCase process result view
 */
export function submitVoiceTranscript(
	text: string,
	recordingId: string | undefined,
	voiceTarget: { sessionId: string | null; selectionVersion: number },
	retryFileId?: string,
): Promise<TranscriptSubmissionResultView> {
	return submitTranscript(text, {
		voice: true,
		recordingId,
		voiceTarget,
		retryFileId,
		reducer: appSessionReducer,
	});
}
