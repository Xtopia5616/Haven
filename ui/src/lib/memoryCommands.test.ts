import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '$lib/tauri.ts';
import type { Fact } from './contracts/memory.ts';
import { addFact, deleteFact, listFacts, recallMemory } from './memoryCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const validFactResponse = (overrides: Record<string, unknown> = {}) => ({
	fact_id: 'fact-00000000000000000000000000000001',
	subject: 'user',
	predicate: 'likes',
	object: 'tea',
	source: 'user',
	confidence: 1,
	tags: [],
	created_at: '2026-09-26T00:00:00Z',
	mention_count: 0,
	last_seen_at: null,
	source_ref: null,
	durability: 1,
	...overrides,
});

const validRecallItem = (overrides: Record<string, unknown> = {}) => ({
	entity_id: 'fact-00000000000000000000000000000003',
	text: 'likes tea',
	score: 0.9,
	model: 'embed-v1',
	...overrides,
});

describe('memory command boundary', () => {
	beforeEach(() => invokeMock.mockReset());

	it('validates fact identity and passes the response fields through unchanged', async () => {
		const response = [
			{
				fact_id: 'fact-00000000000000000000000000000001',
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
		const addedFact = {
			fact_id: 'fact-00000000000000000000000000000002',
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			source: 'user',
			confidence: 1,
			tags: [],
			created_at: '2026-09-26T00:00:00Z',
			mention_count: 0,
			last_seen_at: null,
			source_ref: null,
			durability: 1,
		};
		invokeMock.mockResolvedValueOnce(addedFact as never);
		await expect(
			addFact({
				subject: 'user',
				predicate: 'likes',
				object: 'tea',
				tags: null,
			}),
		).resolves.toBe(addedFact);
		expect(invokeMock).toHaveBeenNthCalledWith(1, 'add_fact', {
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			tags: null,
		});

		invokeMock.mockResolvedValueOnce(undefined);
		const factId = 'fact-00000000000000000000000000000008';
		await deleteFact({ factId });
		expect(invokeMock).toHaveBeenNthCalledWith(2, 'delete_fact', { factId });
	});

	it('validates recall entity IDs and preserves additive response fields', async () => {
		const response = [
			{
				entity_id: 'fact-00000000000000000000000000000003',
				text: 'likes tea',
				score: 0.9,
				model: 'embed-v1',
				extra: true,
			},
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

	it('rejects invalid fact and source-message IDs from Memory commands', async () => {
		invokeMock.mockResolvedValueOnce([{ fact_id: 'fact-short', source_ref: null }] as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce([
			{ id: 'fact-00000000000000000000000000000004', source_ref: null },
		] as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce([
			{
				fact_id: 'fact-00000000000000000000000000000004',
				source_ref: { message_id: 'ses-00000000000000000000000000000005' },
			},
		] as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce([
			{
				fact_id: 'fact-00000000000000000000000000000004',
				source_ref: { message_id: '', snippet: 'orphaned evidence' },
			},
		] as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');
	});

	it('rejects incomplete and out-of-domain fact response fields', async () => {
		const missingSubject = validFactResponse();
		delete (missingSubject as Record<string, unknown>).subject;
		const invalidRows = [
			missingSubject,
			validFactResponse({ tags: 'tea' }),
			validFactResponse({ source: 'imported' }),
			validFactResponse({ confidence: -0.01 }),
			validFactResponse({ confidence: 1.01 }),
			validFactResponse({ durability: -0.01 }),
			validFactResponse({ durability: 1.01 }),
			validFactResponse({ mention_count: -1 }),
			validFactResponse({ mention_count: Number.MAX_SAFE_INTEGER + 1 }),
			validFactResponse({
				source_ref: {
					message_id: 'msg-00000000000000000000000000000002',
				},
			}),
		];

		for (const row of invalidRows) {
			invokeMock.mockResolvedValueOnce([row] as never);
			await expect(listFacts({ source: null })).rejects.toThrow(
				/Invalid memory fact response (shape|value)/,
			);
		}
	});

	it('uses the requested kind and fact as the omitted-kind default for recall IDs', async () => {
		invokeMock.mockResolvedValueOnce([
			{ entity_id: 'msg-00000000000000000000000000000006' },
		] as never);
		await expect(recallMemory({ query: 'tea', kind: 'fact', limit: 5 })).rejects.toThrow(
			'Invalid memory recall identity',
		);

		invokeMock.mockResolvedValueOnce([
			{ entity_id: 'msg-00000000000000000000000000000007' },
		] as never);
		await expect(recallMemory({ query: 'tea', limit: 5 })).rejects.toThrow(
			'Invalid memory recall identity',
		);

		const episodeId = 'msg-00000000000000000000000000000009';
		invokeMock.mockResolvedValueOnce([validRecallItem({ entity_id: episodeId })] as never);
		await expect(recallMemory({ query: 'tea', kind: 'episode', limit: 5 })).resolves.toEqual([
			validRecallItem({ entity_id: episodeId }),
		]);
	});

	it('validates all recall fields while allowing additive fields', async () => {
		for (const item of [
			{ ...validRecallItem(), text: undefined },
			{ ...validRecallItem(), score: Number.POSITIVE_INFINITY },
			{ ...validRecallItem(), model: 42 },
		]) {
			invokeMock.mockResolvedValueOnce([item] as never);
			await expect(recallMemory({ query: 'tea', kind: 'fact', limit: 5 })).rejects.toThrow(
				'Invalid memory recall response shape',
			);
		}

		const response = [validRecallItem({ extra: true, text: '', model: '' })];
		invokeMock.mockResolvedValueOnce(response as never);
		await expect(recallMemory({ query: 'tea', kind: 'fact', limit: 5 })).resolves.toBe(
			response,
		);
	});
});
