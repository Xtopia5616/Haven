import { describe, expect, it } from 'vitest';
import {
	MEMORY_RECALL_FILTER_VALUES,
	isMemoryFactSourceInput,
	isMemoryRecallFilter,
	parseFactResponse,
	parseMemoryRecallResponses,
} from './memory.ts';
import {
	MEMORY_ENTITY_KIND_INPUT_VALUES,
	MEMORY_FACT_SOURCE_INPUT_VALUES,
} from './generatedCommands.ts';

describe('memory recall filter contract', () => {
	it('extends the generated domain kinds with the UI-only all filter', () => {
		expect(MEMORY_RECALL_FILTER_VALUES).toEqual(['all', ...MEMORY_ENTITY_KIND_INPUT_VALUES]);
	});

	it('accepts only declared memory recall filters', () => {
		for (const value of MEMORY_RECALL_FILTER_VALUES) {
			expect(isMemoryRecallFilter(value)).toBe(true);
		}
		expect(isMemoryRecallFilter('unknown')).toBe(false);
	});
});

describe('memory fact source input contract', () => {
	it('accepts only generated source values', () => {
		for (const value of MEMORY_FACT_SOURCE_INPUT_VALUES) {
			expect(isMemoryFactSourceInput(value)).toBe(true);
		}
		expect(isMemoryFactSourceInput('')).toBe(false);
		expect(isMemoryFactSourceInput('future-source')).toBe(false);
	});
});

describe('memory renderer DTO contract', () => {
	it('maps generated fact and recall IDs to camelCase at the boundary', () => {
		const fact = parseFactResponse({
			fact_id: 'fact-00000000000000000000000000000001',
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			source: 'user',
			confidence: 1,
			tags: [],
			created_at: '2026-10-12T08:00:00Z',
			mention_count: 1,
			last_seen_at: null,
			source_ref: {
				message_id: 'msg-00000000000000000000000000000002',
				snippet: 'tea',
			},
			durability: 1,
		});
		expect(fact).toHaveProperty('factId', 'fact-00000000000000000000000000000001');
		expect(fact).not.toHaveProperty('fact_id');
		expect(fact.sourceRef).toEqual({
			messageId: 'msg-00000000000000000000000000000002',
			snippet: 'tea',
		});
		expect(fact.sourceRef).not.toHaveProperty('message_id');

		const recall = parseMemoryRecallResponses(
			[
				{
					entity_id: 'fact-00000000000000000000000000000001',
					text: 'likes tea',
					score: 0.9,
					model: 'embed-v1',
				},
			],
			'fact',
		);
		expect(recall[0]).toHaveProperty('entityId', 'fact-00000000000000000000000000000001');
		expect(recall[0]).not.toHaveProperty('entity_id');
	});
});
