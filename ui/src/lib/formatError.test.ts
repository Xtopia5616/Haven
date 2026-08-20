import { describe, expect, it } from 'vitest';
import { formatError } from './formatError.ts';

describe('formatError', () => {
	it('returns strings as-is', () => {
		expect(formatError('boom')).toBe('boom');
	});

	it('prefers Error.message', () => {
		expect(formatError(new Error('nope'))).toBe('nope');
	});

	it('reads message from plain objects', () => {
		expect(formatError({ message: 'obj' })).toBe('obj');
	});

	it('stringifies other values', () => {
		expect(formatError(42)).toBe('42');
		expect(formatError(null)).toBe('null');
	});
});
