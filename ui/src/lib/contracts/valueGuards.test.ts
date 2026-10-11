import { describe, expect, it } from 'vitest';
import { isSafeInteger, isSafeUnsignedInteger } from './valueGuards.ts';

describe('safe integer guards', () => {
	it('accepts exact signed integer boundaries', () => {
		expect(isSafeInteger(Number.MAX_SAFE_INTEGER)).toBe(true);
		expect(isSafeInteger(-Number.MAX_SAFE_INTEGER)).toBe(true);
	});

	it('rejects fractional, non-finite, and unsafe integers', () => {
		for (const value of [
			1.5,
			Number.NaN,
			Number.POSITIVE_INFINITY,
			Number.MAX_SAFE_INTEGER + 1,
		]) {
			expect(isSafeInteger(value)).toBe(false);
		}
	});

	it('keeps the unsigned guard non-negative', () => {
		expect(isSafeUnsignedInteger(0)).toBe(true);
		expect(isSafeUnsignedInteger(-1)).toBe(false);
	});
});
