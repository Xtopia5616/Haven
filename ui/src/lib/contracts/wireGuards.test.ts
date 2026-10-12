import { describe, expect, it } from 'vitest';
import {
	canonicalIdField,
	isCanonicalEntityId,
	optionalCanonicalIdFieldIsValid,
} from './wireGuards.ts';

describe('canonical entity id wire guards', () => {
	const sessionId = 'ses-103ac63490bc5f6c27fd45405efecd69';

	it('requires the requested prefix and lowercase UUID32 suffix', () => {
		expect(isCanonicalEntityId(sessionId, 'ses')).toBe(true);
		expect(isCanonicalEntityId('toolrun-58ec300e6432076c8ec176f56120ec01', 'toolrun')).toBe(
			true,
		);
		for (const value of [
			'step-455572844fd881c7ae3ae7d45163af1a',
			'ses-7b',
			'ses-0123456789abcdef0123456789abcdeF',
			'ses-0000000000000000000000000000000Z',
			'ses-not-an-id',
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
