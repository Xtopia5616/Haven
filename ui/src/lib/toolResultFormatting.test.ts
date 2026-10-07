import { describe, expect, it } from 'vitest';
import { clampPercentage, formatByteSize } from './toolResultFormatting.ts';

describe('tool result formatting', () => {
	it.each([
		[0, '0 B'],
		[1023, '1023 B'],
		[1024, '1.0 KB'],
		[1024 * 100, '100 KB'],
		[-1, '—'],
		[Number.POSITIVE_INFINITY, '—'],
	])('formats byte size %s as %s', (value, expected) => {
		expect(formatByteSize(value)).toBe(expected);
	});

	it.each([
		[-10, 0],
		['37.5', 37.5],
		[125, 100],
		[Number.NaN, 0],
	])('clamps percentage %s to %s', (value, expected) => {
		expect(clampPercentage(value)).toBe(expected);
	});
});
