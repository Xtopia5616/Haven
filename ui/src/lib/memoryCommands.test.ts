import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '$lib/tauri.ts';
import type { Fact } from './contracts/memory.ts';
import { addFact, deleteFact, listFacts, recallMemory } from './memoryCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);

describe('memory command boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('passes fact list request and response through without changing wire fields', async () => {
		const response = [
			{
				id: 'fact-1',
				subject: 'user',
				predicate: 'likes',
				object: 'tea',
				source: 'user' as const,
				confidence: 1,
				tags: [],
				created_at: '2026-09-26T00:00:00Z',
				mention_count: 0,
				last_seen_at: null,
				source_ref: null,
				durability: 1,
				future_field: 'retained',
			},
		];
		invokeMock.mockResolvedValue(response as unknown as Fact[]);

		await expect(listFacts({ source: null })).resolves.toBe(response);
		expect(invokeMock).toHaveBeenCalledWith('list_facts', { source: null });
	});

	it('passes add and delete request fields unchanged', async () => {
		invokeMock.mockResolvedValueOnce(undefined);
		await addFact({
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			tags: null,
		});
		expect(invokeMock).toHaveBeenNthCalledWith(1, 'add_fact', {
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			tags: null,
		});

		invokeMock.mockResolvedValueOnce(undefined);
		await deleteFact({ factId: 'fact-1' });
		expect(invokeMock).toHaveBeenNthCalledWith(2, 'delete_fact', { factId: 'fact-1' });
	});

	it('passes recall fields and additive response data through unchanged', async () => {
		const response = [
			{ entity_id: 'fact-1', text: 'likes tea', score: 0.9, model: 'embed-v1', extra: true },
		];
		invokeMock.mockResolvedValue(response);

		await expect(recallMemory({ query: 'tea', kind: 'fact', limit: 10 })).resolves.toBe(
			response,
		);
		expect(invokeMock).toHaveBeenCalledWith('recall_memory', {
			query: 'tea',
			kind: 'fact',
			limit: 10,
		});
	});
});
