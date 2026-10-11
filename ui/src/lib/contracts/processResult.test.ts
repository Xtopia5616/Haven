import { describe, expect, it } from 'vitest';
import { parseProcessResult } from './processResult.ts';

const sessionId = 'ses-00000000000000000000000000000001';
const messageId = 'msg-00000000000000000000000000000002';

describe('process transcript response identity contract', () => {
	it('accepts canonical session and optional message identities', () => {
		const created = { SessionCreated: { session_id: sessionId, message_id: messageId } };
		const supplemented = { Supplemented: { message_id: null } };
		expect(parseProcessResult(created)).toBe(created);
		expect(parseProcessResult(supplemented)).toBe(supplemented);
	});

	it('rejects malformed variants and non-canonical identity fields', () => {
		for (const value of [
			{},
			{ SessionCreated: { session_id: 'ses-short' } },
			{
				SessionCreated: {
					session_id: sessionId,
					message_id: 'step-00000000000000000000000000000002',
				},
			},
			{ Supplemented: { message_id: 'msg-short' } },
			{ SessionCreated: { session_id: sessionId }, Supplemented: {} },
		]) {
			expect(() => parseProcessResult(value)).toThrow('Invalid process transcript identity');
		}
	});
});
