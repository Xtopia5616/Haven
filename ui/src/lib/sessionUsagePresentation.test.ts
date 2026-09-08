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
});
