import { describe, expect, it } from 'vitest';
import { buildTokenUsageDetails, estimateToolDataTokens } from './sessionUsagePresentation.ts';

describe('estimateToolDataTokens', () => {
	it('keeps each tool payload separate from model-step usage', () => {
		const usage = estimateToolDataTokens('shell', { command: 'dir' }, 'file.txt');

		expect(usage).not.toBeNull();
		expect(usage?.args).toBeGreaterThan(0);
		expect(usage?.result).toBeGreaterThan(0);
		expect(usage?.total).toBe((usage?.args ?? 0) + (usage?.result ?? 0));
	});

	it('counts CJK result text without treating it as four ASCII characters', () => {
		const usage = estimateToolDataTokens('shell', null, '中文结果');

		expect(usage?.result).toBe(4);
	});
});

describe('buildTokenUsageDetails', () => {
	it('shows inclusive cache rate and context budget for a live call', () => {
		const details = buildTokenUsageDetails(
			{
				promptTokens: 200,
				completionTokens: 50,
				totalTokens: 250,
				cachedTokens: 100,
				cacheMissTokens: 100,
				cacheAccounting: 'inclusive',
				contextTokens: 200,
				contextWindow: 1000,
				cumulativePromptTokens: 200,
				cumulativeCompletionTokens: 50,
				cumulativeTotalTokens: 250,
			},
			[],
		);

		expect(details.currentCacheRatePercent).toBe(50);
		expect(details.contextRatePercent).toBe(20);
		expect(details.currentPromptTokens).toBe(200);
	});

	it('uses the last persisted call for restored context details', () => {
		const details = buildTokenUsageDetails(
			{
				restored: true,
				cumulativePromptTokens: 400,
				cumulativeCompletionTokens: 80,
				cumulativeTotalTokens: 480,
			},
			[
				{
					call_kind: 'agent',
					prompt_tokens: 300,
					completion_tokens: 40,
					total_tokens: 340,
					context_tokens: 900,
					context_window: 2000,
					cache_accounting: 'exclusive',
					cached_tokens: 200,
				},
			],
		);

		expect(details.currentPromptTokens).toBe(300);
		expect(details.contextTokens).toBe(900);
		expect(details.contextRatePercent).toBe(45);
		expect(details.currentCacheRatePercent).toBe(40);
	});

	it('does not guess a cache rate for unknown accounting', () => {
		const details = buildTokenUsageDetails(
			{ promptTokens: 100, cachedTokens: 80, totalTokens: 180 },
			[],
		);

		expect(details.currentCacheRatePercent).toBeNull();
	});

	it('keeps a known zero cache hit rate visible', () => {
		const details = buildTokenUsageDetails(
			{ promptTokens: 100, cachedTokens: 0, cacheAccounting: 'inclusive' },
			[],
		);

		expect(details.currentCacheRatePercent).toBe(0);
	});

	it('reports media inference separately without changing Agent totals', () => {
		const details = buildTokenUsageDetails(
			{
				promptTokens: 100,
				completionTokens: 20,
				totalTokens: 120,
				cachedTokens: 50,
				cacheAccounting: 'inclusive',
				cumulativePromptTokens: 100,
				cumulativeCompletionTokens: 20,
				cumulativeCachedTokens: 50,
				cumulativeTotalTokens: 120,
				cumulativeCostUsd: 0.01,
			},
			[
				{
					call_kind: 'agent',
					prompt_tokens: 100,
					completion_tokens: 20,
					total_tokens: 120,
					cached_tokens: 50,
					cache_accounting: 'inclusive',
				},
				{
					call_kind: 'media',
					prompt_tokens: 800,
					completion_tokens: 100,
					total_tokens: 900,
					cache_accounting: 'unknown',
					cost_usd: 0.02,
					has_cost: true,
				},
				{
					call_kind: 'tool',
					prompt_tokens: 40,
					completion_tokens: 10,
					total_tokens: 50,
					cache_accounting: 'unknown',
					cost_usd: 0.01,
					has_cost: true,
				},
			],
		);

		expect(details.callCount).toBe(1);
		expect(details.mediaCallCount).toBe(1);
		expect(details.mediaTotalTokens).toBe(900);
		expect(details.mediaCostUsd).toBe(0.02);
		expect(details.toolCallCount).toBe(1);
		expect(details.toolTotalTokens).toBe(50);
		expect(details.toolCostUsd).toBe(0.01);
		expect(details.cumulativeCacheRatePercent).toBe(50);
		expect(details.cumulativeTotalTokens).toBe(120);
	});
});
