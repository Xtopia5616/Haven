import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '$lib/tauri.ts';
import {
	addFact,
	deleteFact,
	listFactProposals,
	listFacts,
	recallMemory,
	reviewFactProposal,
} from './memoryCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const validFactResponse = (overrides: Record<string, unknown> = {}) => ({
	fact_id: 'fact-00000000000000000000000000000001',
	subject: 'user',
	predicate: 'likes',
	object: 'tea',
	source: 'user',
	memory_kind: 'profile',
	confidence: 1,
	tags: [],
	created_at: '2026-09-26T00:00:00Z',
	mention_count: 0,
	last_seen_at: null,
	source_ref: null,
	durability: 1,
	valid_from: null,
	valid_to: null,
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

	it('maps fact IDs to camelCase while preserving the other response fields', async () => {
		const response = {
			facts: [
				{
					fact_id: 'fact-00000000000000000000000000000001',
					subject: 'user',
					predicate: 'likes',
					object: 'tea',
					source: 'user' as const,
					memory_kind: 'profile' as const,
					confidence: 1,
					tags: [],
					created_at: '2026-09-26T00:00:00Z',
					mention_count: 0,
					last_seen_at: null,
					source_ref: null,
					durability: 1,
					valid_from: null,
					valid_to: null,
					future_field: 'retained',
				},
			],
			next_cursor: null,
		};
		invokeMock.mockResolvedValue(response as never);

		const page = await listFacts({ source: null });
		expect(page).toEqual({
			facts: [
				{
					factId: response.facts[0]!.fact_id,
					subject: 'user',
					predicate: 'likes',
					object: 'tea',
					source: 'user',
					memory_kind: 'profile',
					confidence: 1,
					tags: [],
					created_at: '2026-09-26T00:00:00Z',
					mention_count: 0,
					last_seen_at: null,
					sourceRef: null,
					durability: 1,
					valid_from: null,
					valid_to: null,
					future_field: 'retained',
				},
			],
			nextCursor: null,
		});
		expect(page.facts[0]).not.toHaveProperty('fact_id');
		expect(invokeMock).toHaveBeenCalledWith('list_facts', { source: null });
	});

	it('lists and reviews inferred fact proposals through validated command contracts', async () => {
		const proposalId = 'fprop-00000000000000000000000000000001';
		const messageId = 'msg-00000000000000000000000000000002';
		invokeMock.mockResolvedValueOnce({
			proposals: [
				{
					proposal_id: proposalId,
					subject: 'user',
					predicate: 'likes',
					object: 'tea',
					confidence: 0.8,
					tags: ['preference'],
					durability: 0.5,
					created_at: '2026-10-12T08:00:00.000Z',
					source_ref: { message_id: messageId, snippet: 'I like tea' },
					evidence_status: 'uncertain',
					evidence_quote: null,
					valid_from: null,
					valid_to: null,
				},
			],
			next_cursor: null,
		} as never);
		const page = await listFactProposals({ cursor: null, limit: 50 });
		expect(page.proposals[0]).toMatchObject({ proposalId, sourceRef: { messageId } });
		expect(invokeMock).toHaveBeenNthCalledWith(1, 'list_fact_proposals', {
			cursor: null,
			limit: 50,
		});

		invokeMock.mockResolvedValueOnce({ status: 'accepted', fact_written: true });
		await expect(reviewFactProposal({ proposalId, decision: 'accept' })).resolves.toEqual({
			status: 'accepted',
			fact_written: true,
		});
		expect(invokeMock).toHaveBeenNthCalledWith(2, 'review_fact_proposal', {
			proposalId,
			decision: 'accept',
		});
	});

	it('passes add and delete request fields unchanged', async () => {
		const addedFact = {
			fact_id: 'fact-00000000000000000000000000000002',
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			source: 'user',
			memory_kind: 'profile',
			confidence: 1,
			tags: [],
			created_at: '2026-09-26T00:00:00Z',
			mention_count: 0,
			last_seen_at: null,
			source_ref: null,
			durability: 1,
			valid_from: null,
			valid_to: null,
		};
		invokeMock.mockResolvedValueOnce(addedFact as never);
		await expect(
			addFact({
				subject: 'user',
				predicate: 'likes',
				object: 'tea',
				tags: null,
			}),
		).resolves.toMatchObject({ factId: addedFact.fact_id });
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

	it('maps recall entity IDs and preserves additive response fields', async () => {
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

		const results = await recallMemory({ query: 'tea', kind: 'fact', limit: 10 });
		expect(results).toEqual([
			{
				entityId: response[0]!.entity_id,
				text: 'likes tea',
				score: 0.9,
				model: 'embed-v1',
				extra: true,
			},
		]);
		expect(results[0]).not.toHaveProperty('entity_id');
		expect(invokeMock).toHaveBeenCalledWith('recall_memory', {
			query: 'tea',
			kind: 'fact',
			limit: 10,
		});
	});

	it('rejects invalid fact and source-message IDs from Memory commands', async () => {
		invokeMock.mockResolvedValueOnce({
			facts: [{ fact_id: 'fact-short', source_ref: null }],
			next_cursor: null,
		} as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce({
			facts: [{ id: 'fact-00000000000000000000000000000004', source_ref: null }],
			next_cursor: null,
		} as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce({
			facts: [
				{
					fact_id: 'fact-00000000000000000000000000000004',
					source_ref: { message_id: 'ses-00000000000000000000000000000005' },
				},
			],
			next_cursor: null,
		} as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact identity');

		invokeMock.mockResolvedValueOnce({
			facts: [
				{
					fact_id: 'fact-00000000000000000000000000000004',
					source_ref: { message_id: '', snippet: 'orphaned evidence' },
				},
			],
			next_cursor: null,
		} as never);
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
			invokeMock.mockResolvedValueOnce({ facts: [row], next_cursor: null } as never);
			await expect(listFacts({ source: null })).rejects.toThrow(
				/Invalid memory fact response (shape|value)/,
			);
		}
	});

	it('maps and validates stable fact page cursors', async () => {
		const cursor = {
			createdAt: '2026-09-26T00:00:00Z',
			factId: 'fact-00000000000000000000000000000004',
		};
		invokeMock.mockResolvedValueOnce({
			facts: [validFactResponse()],
			next_cursor: cursor,
		} as never);
		await expect(listFacts({ source: null })).resolves.toMatchObject({
			facts: [{ factId: 'fact-00000000000000000000000000000001' }],
			nextCursor: cursor,
		});

		invokeMock.mockResolvedValueOnce({
			facts: [],
			next_cursor: { createdAt: '2026-09-26T00:00:00Z', factId: 'fact-short' },
		} as never);
		await expect(listFacts({ source: null })).rejects.toThrow('Invalid memory fact cursor');
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
			{ entityId: episodeId, text: 'likes tea', score: 0.9, model: 'embed-v1' },
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
		await expect(recallMemory({ query: 'tea', kind: 'fact', limit: 5 })).resolves.toEqual([
			{ entityId: response[0]!.entity_id, text: '', score: 0.9, model: '', extra: true },
		]);
	});
});
