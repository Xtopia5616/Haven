import { describe, expect, it } from 'vitest';
import { parseBooleanResponse } from './boolean.ts';

describe('parseBooleanResponse', () => {
	it.each([true, false])('accepts the boolean response %s', (value) => {
		expect(parseBooleanResponse(value, 'test_command')).toBe(value);
	});

	it.each([null, 0, 1, 'true', {}, []])('rejects a malformed boolean response: %j', (value) => {
		expect(() => parseBooleanResponse(value, 'test_command')).toThrow(
			'Invalid test_command boolean response',
		);
	});
});
