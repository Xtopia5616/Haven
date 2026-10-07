import { describe, expect, it } from 'vitest';
import { isRecord } from './objectGuards.ts';

describe('isRecord', () => {
	it('accepts objects and rejects arrays, null, and primitives', () => {
		expect(isRecord({ status: 'ready' })).toBe(true);
		expect(isRecord(Object.create(null))).toBe(true);
		expect(isRecord([])).toBe(false);
		expect(isRecord(null)).toBe(false);
		expect(isRecord('ready')).toBe(false);
		expect(isRecord(1)).toBe(false);
	});
});
