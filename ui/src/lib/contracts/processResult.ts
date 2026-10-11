/** Runtime identity validation for the generated process_transcript response. */

import type { ProcessResult } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

function hasOptionalMessageId(record: Record<string, unknown>): boolean {
	const messageId = record.message_id;
	return messageId === undefined || messageId === null || isCanonicalEntityId(messageId, 'msg');
}

/** Reject malformed command results before they can select or re-key session state. */
export function parseProcessResult(value: unknown): ProcessResult {
	if (!isRecord(value)) throw new Error('Invalid process transcript identity');
	const hasCreated = Object.prototype.hasOwnProperty.call(value, 'SessionCreated');
	const hasSupplemented = Object.prototype.hasOwnProperty.call(value, 'Supplemented');
	if (hasCreated === hasSupplemented) throw new Error('Invalid process transcript identity');

	if (hasCreated) {
		const created = value.SessionCreated;
		if (
			!isRecord(created) ||
			!isCanonicalEntityId(created.session_id, 'ses') ||
			!hasOptionalMessageId(created)
		) {
			throw new Error('Invalid process transcript identity');
		}
	} else {
		const supplemented = value.Supplemented;
		if (!isRecord(supplemented) || !hasOptionalMessageId(supplemented)) {
			throw new Error('Invalid process transcript identity');
		}
	}

	return value as unknown as ProcessResult;
}
