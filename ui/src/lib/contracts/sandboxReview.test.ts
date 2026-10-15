import { describe, expect, it } from 'vitest';
import { parseSandboxOutputReview } from './sandboxReview.ts';

const review = () => ({
	sessionId: 'ses-00000000000000000000000000000002',
	sourceStepId: 'step-00000000000000000000000000000001',
	expiresAtUnixMs: 1_800_000_000_000,
	storageChargeBytes: 128,
	changes: [
		{
			changeIndex: 0,
			relativePath: 'reports/result.txt',
			change: 'created',
			beforeBytes: null,
			afterBytes: 12,
			beforeSha256: null,
			afterSha256: 'a'.repeat(64),
			promoted: false,
		},
	],
});

describe('parseSandboxOutputReview', () => {
	it('accepts the bounded renderer projection and null', () => {
		expect(parseSandboxOutputReview(review())?.changes[0].relativePath).toBe('reports/result.txt');
		expect(parseSandboxOutputReview(null)).toBeNull();
	});

	it.each(['../outside.txt', 'C:\\private\\file.txt', '/absolute/file.txt', 'a//b.txt'])(
		'rejects unsafe display paths: %s',
		(relativePath) => {
			const value = review();
			value.changes[0].relativePath = relativePath;
			expect(() => parseSandboxOutputReview(value)).toThrow('Invalid sandbox output review change');
		},
	);

	it('rejects duplicate indexes, malformed hashes, and unknown change kinds', () => {
		const duplicate = review();
		duplicate.changes.push({ ...duplicate.changes[0] });
		expect(() => parseSandboxOutputReview(duplicate)).toThrow();

		const malformedHash = review();
		malformedHash.changes[0].afterSha256 = 'not-a-hash';
		expect(() => parseSandboxOutputReview(malformedHash)).toThrow();

		const unknownKind = review();
		unknownKind.changes[0].change = 'copied';
		expect(() => parseSandboxOutputReview(unknownKind)).toThrow();
	});
});
