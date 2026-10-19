/** Runtime validation and renderer projection for transcript submission responses. */

import { isRecord } from './objectGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

export type TranscriptSubmissionResultView =
	| { kind: 'session_created'; sessionId: string; messageId?: string | null }
	| { kind: 'supplemented'; sessionId: string; messageId?: string | null };

function optionalMessageId(record: Record<string, unknown>): string | null | undefined {
	const messageId = record.message_id;
	if (messageId === undefined || messageId === null) return messageId;
	if (isCanonicalEntityId(messageId, 'msg')) return messageId;
	throw new Error('Invalid transcript submission identity');
}

/** Validate Rust wire identity, then return a camelCase renderer view. */
export function parseTranscriptSubmissionResult(
	value: unknown,
	expectedSessionId?: string | null,
): TranscriptSubmissionResultView {
	if (!isRecord(value)) throw new Error('Invalid transcript submission identity');
	const hasCreated = Object.prototype.hasOwnProperty.call(value, 'SessionCreated');
	const hasSupplemented = Object.prototype.hasOwnProperty.call(value, 'Supplemented');
	if (hasCreated === hasSupplemented) throw new Error('Invalid transcript submission identity');

	if (hasCreated) {
		const created = value.SessionCreated;
		if (!isRecord(created) || !isCanonicalEntityId(created.session_id, 'ses')) {
			throw new Error('Invalid transcript submission identity');
		}
		const messageId = optionalMessageId(created);
		return {
			kind: 'session_created',
			sessionId: created.session_id,
			...(messageId === undefined ? {} : { messageId }),
		};
	} else {
		const supplemented = value.Supplemented;
		if (
			!isRecord(supplemented) ||
			!isCanonicalEntityId(supplemented.session_id, 'ses') ||
			supplemented.session_id !== expectedSessionId
		) {
			throw new Error('Invalid transcript submission identity');
		}
		const messageId = optionalMessageId(supplemented);
		return {
			kind: 'supplemented',
			sessionId: supplemented.session_id,
			...(messageId === undefined ? {} : { messageId }),
		};
	}
}
