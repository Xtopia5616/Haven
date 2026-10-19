import { describe, expect, it } from 'vitest';
import { parseTranscriptSubmissionResult } from './transcriptSubmissionResult.ts';

const sessionId = 'ses-00000000000000000000000000000001';
const messageId = 'msg-00000000000000000000000000000002';

describe('transcript submission response identity contract', () => {
	it('accepts canonical session and optional message identities', () => {
		const created = { SessionCreated: { session_id: sessionId, message_id: messageId } };
		const supplemented = { Supplemented: { session_id: sessionId, message_id: null } };
		expect(parseTranscriptSubmissionResult(created)).toEqual({
			kind: 'session_created',
			sessionId,
			messageId,
		});
		expect(parseTranscriptSubmissionResult(created)).not.toBe(created);
		expect(parseTranscriptSubmissionResult(created)).not.toHaveProperty('SessionCreated');
		expect(parseTranscriptSubmissionResult(supplemented, sessionId)).toEqual({
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
			expect(() => parseTranscriptSubmissionResult(value)).toThrow(
				'Invalid transcript submission identity',
			);
		}
	});

	it('requires supplemented results to belong to the submitted session', () => {
		const supplemented = {
			Supplemented: {
				session_id: 'ses-00000000000000000000000000000003',
				message_id: messageId,
			},
		};
		expect(() => parseTranscriptSubmissionResult(supplemented, sessionId)).toThrow(
			'Invalid transcript submission identity',
		);
		expect(() => parseTranscriptSubmissionResult(supplemented, null)).toThrow(
			'Invalid transcript submission identity',
		);
	});
});
