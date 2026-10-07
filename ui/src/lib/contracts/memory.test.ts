import { describe, expect, it } from 'vitest';
import {
	MEMORY_RECALL_FILTER_VALUES,
	isMemoryRecallFilter,
} from './memory.ts';
import { MEMORY_ENTITY_KIND_INPUT_VALUES } from './generatedCommands.ts';

describe('memory recall filter contract', () => {
	it('extends the generated domain kinds with the UI-only all filter', () => {
		expect(MEMORY_RECALL_FILTER_VALUES).toEqual([
			'all',
			...MEMORY_ENTITY_KIND_INPUT_VALUES,
		]);
	});

	it('accepts only declared memory recall filters', () => {
		for (const value of MEMORY_RECALL_FILTER_VALUES) {
			expect(isMemoryRecallFilter(value)).toBe(true);
		}
		expect(isMemoryRecallFilter('unknown')).toBe(false);
	});
});
