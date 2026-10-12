import { describe, expect, it } from 'vitest';
import { parseProcessResult } from './processResult.ts';

const sessionId = 'ses-00000000000000000000000000000001';
const messageId = 'msg-00000000000000000000000000000002';

describe('process transcript response identity contract', () => {
	it('accepts canonical session and optional message identities', () => {
		const created = { SessionCreated: { session_id: sessionId, message_id: messageId } };
		const supplemented = { Supplemented: { session_id: sessionId, message_id: null } };
		expect(parseProcessResult(created)).toEqual({
			kind: 'session_created',
			sessionId,
			messageId,
		});
		expect(parseProcessResult(created)).not.toBe(created);
		expect(parseProcessResult(created)).not.toHaveProperty('SessionCreated');
		expect(parseProcessResult(supplemented, sessionId)).toEqual({
			kind: 'supplemented',
			sessionId,
			messageId: null,
		});
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
			{ Supplemented: { session_id: sessionId, message_id: 'msg-short' } },
			{ SessionCreated: { session_id: sessionId }, Supplemented: {} },
		]) {
			expect(() => parseProcessResult(value)).toThrow('Invalid process transcript identity');
		}
	});

	it('requires supplemented results to belong to the submitted session', () => {
		const supplemented = {
			Supplemented: {
				session_id: 'ses-00000000000000000000000000000003',
				message_id: messageId,
			},
		};
		expect(() => parseProcessResult(supplemented, sessionId)).toThrow(
			'Invalid process transcript identity',
		);
		expect(() => parseProcessResult(supplemented, null)).toThrow(
			'Invalid process transcript identity',
		);
	});
});
