import { describe, expect, it } from 'vitest';
import { isFirstToolForStep, stepUsageFor } from './sessionUsagePresentation.ts';

describe('isFirstToolForStep', () => {
	it('renders one aggregate for parallel tools in the same step', () => {
		const messages = [
			{ type: 'tool', stepNumber: 3 },
			{ type: 'tool', stepNumber: 3 },
			{ type: 'tool', stepNumber: 4 },
		];

		expect(isFirstToolForStep(messages, 0)).toBe(true);
		expect(isFirstToolForStep(messages, 1)).toBe(false);
		expect(isFirstToolForStep(messages, 2)).toBe(true);
	});

	it('ignores non-tool messages and tools without a step number', () => {
		const messages = [
			{ type: 'assistant', stepNumber: 1 },
			{ type: 'tool', stepNumber: null },
		];

		expect(isFirstToolForStep(messages, 0)).toBe(false);
		expect(isFirstToolForStep(messages, 1)).toBe(false);
	});
});

describe('stepUsageFor', () => {
	it('keeps exclusive cache tokens in the provider total', () => {
		const usage = stepUsageFor(
			[
				{
					step_number: 2,
					prompt_tokens: 100,
					completion_tokens: 20,
					total_tokens: 0,
					cached_tokens: 400,
					cache_creation_tokens: 50,
					cache_accounting: 'exclusive',
				},
			],
			2,
			new Map(),
		);

		expect(usage?.total).toBe(570);
	});
});
