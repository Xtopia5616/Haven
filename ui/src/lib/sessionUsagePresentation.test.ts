import { describe, expect, it } from 'vitest';
import { estimateToolDataTokens } from './sessionUsagePresentation.ts';

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
