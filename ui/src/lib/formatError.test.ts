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
		expect(formatError(null)).toBe('未知错误');
		expect(formatError({ code: 'E_FAIL' })).toBe('未知错误');
	});

	it('redacts credentials and local paths', () => {
		expect(formatError('request failed api_key=sk-secret at C:\\Users\\olive\\haven.db')).toBe(
			'request failed api_key=[REDACTED] at [PATH]',
		);
	});

	it('redacts common provider token prefixes', () => {
		expect(formatError('providers returned sk-1234567890 and gsk_1234567890')).toBe(
			'providers returned [REDACTED] and [REDACTED]',
		);
		expect(formatError('azure key AIza123456789012345678901234567890')).toBe(
			'azure key [REDACTED]',
		);
	});

	it('keeps UI messages on one bounded line', () => {
		expect(formatError('line one\nline two\twith spaces')).toBe(
			'line one line two with spaces',
		);
		expect(formatError('x'.repeat(300))).toHaveLength(240);
	});
});
