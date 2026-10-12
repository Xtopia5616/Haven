import { describe, expect, it } from 'vitest';
import { parseCountResponse } from './count.ts';

describe('parseCountResponse', () => {
	it('accepts non-negative JavaScript safe integers', () => {
		expect(parseCountResponse(0, 'clear_facts')).toBe(0);
		expect(parseCountResponse(Number.MAX_SAFE_INTEGER, 'clear_facts')).toBe(
			Number.MAX_SAFE_INTEGER,
		);
	});

	it.each([-1, 1.5, Number.MAX_SAFE_INTEGER + 1, Number.NaN, null, '2'])(
		'rejects invalid count %j',
		(value) => {
			expect(() => parseCountResponse(value, 'clear_facts')).toThrow(
				'Invalid clear_facts count response',
			);
		},
	);
});
