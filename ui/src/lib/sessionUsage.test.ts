import { describe, it, expect } from 'vitest';
import {
	formatTokenCount,
	formatCostUsd,
	coalesceTokenTotal,
	cumulativeCacheHitRatePercent,
} from './sessionUsage.ts';

describe('token usage helpers', () => {
	it('coalesceTokenTotal fills omitted total', () => {
		expect(coalesceTokenTotal(10, 5, 0)).toBe(15);
		expect(coalesceTokenTotal(10, 5, 20)).toBe(20);
		expect(coalesceTokenTotal(0, 0, 0)).toBe(0);
	});

	it('coalesceTokenTotal adds exclusive cache when total is omitted', () => {
		expect(coalesceTokenTotal(100, 20, 0, 400, 50, 'exclusive')).toBe(570);
		expect(coalesceTokenTotal(100, 20, 0, 80, 0)).toBe(120);
		expect(coalesceTokenTotal(100, 20, 125, 80, 0)).toBe(125);
	});

	it('calculates a mixed-provider cache rate from each call contract', () => {
		const rate = cumulativeCacheHitRatePercent([
			{
				call_kind: 'agent',
				prompt_tokens: 100,
				cached_tokens: 100,
				cache_accounting: 'inclusive',
			},
			{
				call_kind: 'agent',
				prompt_tokens: 100,
				cached_tokens: 400,
				cache_accounting: 'exclusive',
			},
		]);
		expect(rate).toBeCloseTo((500 / 600) * 100, 6);
	});

	it('does not guess a cache rate for unknown calls', () => {
		expect(
			cumulativeCacheHitRatePercent([
				{
					call_kind: 'agent',
					prompt_tokens: 100,
					cached_tokens: 80,
					cache_accounting: 'unknown',
				},
			]),
		).toBeNull();
	});

	it('excludes media-owned calls from the Agent cache rate', () => {
		expect(
			cumulativeCacheHitRatePercent([
				{
					call_kind: 'agent',
					prompt_tokens: 100,
					cached_tokens: 50,
					cache_accounting: 'inclusive',
				},
				{
					call_kind: 'media',
					prompt_tokens: 10_000,
					cached_tokens: 0,
					cache_accounting: 'unknown',
				},
			]),
		).toBe(50);
	});
});

describe('formatTokenCount', () => {
	it('formats plain counts without suffix', () => {
		expect(formatTokenCount(0)).toBe('0');
		expect(formatTokenCount(999)).toBe('999');
	});

	it('formats thousands with K suffix', () => {
		expect(formatTokenCount(1234)).toBe('1.23K');
		expect(formatTokenCount(12000)).toBe('12K');
	});

	it('formats millions with M suffix', () => {
		expect(formatTokenCount(1234567)).toBe('1.23M');
	});

	it('tolerates non-numeric input', () => {
		expect(formatTokenCount(undefined as any)).toBe('0');
		expect(formatTokenCount('300' as any)).toBe('300');
	});
});

describe('formatCostUsd', () => {
	it('returns null for missing or non-finite values', () => {
		expect(formatCostUsd(null)).toBeNull();
		expect(formatCostUsd(undefined)).toBeNull();
		expect(formatCostUsd(NaN)).toBeNull();
		expect(formatCostUsd(Infinity)).toBeNull();
	});

	it('formats zero', () => {
		expect(formatCostUsd(0)).toBe('$0.00');
	});

	it('uses 4 decimals for sub-cent costs', () => {
		expect(formatCostUsd(0.00123)).toBe('$0.0012');
	});

	it('uses 3 decimals under one dollar', () => {
		expect(formatCostUsd(0.1234)).toBe('$0.123');
	});

	it('uses 2 decimals for whole dollars', () => {
		expect(formatCostUsd(1.5)).toBe('$1.50');
		expect(formatCostUsd(21)).toBe('$21.00');
	});
});
