import { describe, expect, it } from 'vitest';
import {
	canonicalIdField,
	isCanonicalEntityId,
	optionalCanonicalIdFieldIsValid,
} from './wireGuards.ts';

describe('canonical entity id wire guards', () => {
	const sessionId = 'ses-0123456789abcdef0123456789abcdef';

	it('requires the requested prefix and lowercase UUID32 suffix', () => {
		expect(isCanonicalEntityId(sessionId, 'ses')).toBe(true);
		expect(isCanonicalEntityId('toolrun-0123456789abcdef0123456789abcdef', 'toolrun')).toBe(
			true,
		);
		for (const value of [
			'step-0123456789abcdef0123456789abcdef',
			'ses-0123',
			'ses-01234567-89ab-cdef-0123-456789abcdef',
			'ses-0123456789ABCDEF0123456789abcdef',
			'ses-0123456789abcdef0123456789abcdeg',
			`${sessionId}\n`,
		]) {
			expect(isCanonicalEntityId(value, 'ses')).toBe(false);
		}
		for (const prefix of ['', 'Bad', 'tool-run', 'ses_', '会话']) {
			expect(isCanonicalEntityId(sessionId, prefix)).toBe(false);
		}
	});

	it('reads required and optional ID fields without accepting another entity type', () => {
		const record = { session_id: sessionId };
		expect(canonicalIdField(record, 'session_id', 'ses')).toBe(sessionId);
		expect(canonicalIdField(record, 'session_id', 'msg')).toBeNull();
		expect(optionalCanonicalIdFieldIsValid({}, 'session_id', 'ses')).toBe(true);
		expect(optionalCanonicalIdFieldIsValid(record, 'session_id', 'ses')).toBe(true);
		expect(optionalCanonicalIdFieldIsValid(record, 'session_id', 'toolrun')).toBe(false);
	});
});
