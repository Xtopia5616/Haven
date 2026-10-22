import { describe, expect, it } from 'vitest';
import {
	MEMORY_RECALL_FILTER_VALUES,
	isMemoryFactSourceInput,
	isMemoryRecallFilter,
	parseFactResponse,
	parseFactProposalPageResponse,
	parseFactProposalReviewResponse,
	parseMemoryRecallResponses,
} from './memory.ts';
import {
	MEMORY_RECALL_KIND_INPUT_VALUES,
	MEMORY_FACT_SOURCE_INPUT_VALUES,
} from './generatedCommands.ts';

describe('memory recall filter contract', () => {
	it('extends the generated domain kinds with the UI-only all filter', () => {
		expect(MEMORY_RECALL_FILTER_VALUES).toEqual(['all', ...MEMORY_RECALL_KIND_INPUT_VALUES]);
	});

	it('accepts only declared memory recall filters', () => {
		for (const value of MEMORY_RECALL_FILTER_VALUES) {
			expect(isMemoryRecallFilter(value)).toBe(true);
		}
		expect(isMemoryRecallFilter('unknown')).toBe(false);
	});
});

describe('memory fact proposal contract', () => {
	const proposalId = 'fprop-00000000000000000000000000000001';
	const messageId = 'msg-00000000000000000000000000000002';
	const proposal = {
		proposal_id: proposalId,
		subject: 'user',
		predicate: 'likes',
		object: 'tea',
		confidence: 0.8,
		tags: ['preference'],
		durability: 0.5,
		created_at: '2026-10-12T08:00:00.000Z',
		valid_from: null,
		valid_to: null,
		source_ref: { message_id: messageId, snippet: 'I like tea' },
		evidence_status: 'uncertain',
		evidence_quote: null,
	};

	it('maps proposal and source identities to camelCase', () => {
		const page = parseFactProposalPageResponse({
			proposals: [proposal],
			next_cursor: { createdAt: '2026-10-12T08:00:00.000Z', proposalId },
		});
		expect(page.proposals[0]).toMatchObject({ proposalId, created_at: proposal.created_at });
		expect(page.proposals[0]).not.toHaveProperty('proposal_id');
		expect(page.proposals[0].sourceRef).toEqual({ messageId, snippet: 'I like tea' });
		expect(page.nextCursor).toEqual({ createdAt: proposal.created_at, proposalId });
	});

	it('rejects non-canonical proposal and source identities and invalid confidence', () => {
		expect(() => parseFactProposalPageResponse({ proposals: [{ ...proposal, proposal_id: 'bad' }], next_cursor: null })).toThrow(
			'Invalid memory fact proposal identity',
		);
		expect(() =>
			parseFactProposalPageResponse({
				proposals: [{ ...proposal, source_ref: { message_id: 'msg-bad', snippet: '' } }],
				next_cursor: null,
			}),
		).toThrow('Invalid memory fact proposal identity');
		expect(() =>
			parseFactProposalPageResponse({
				proposals: [{ ...proposal, confidence: 1.1 }],
				next_cursor: null,
			}),
		).toThrow('Invalid memory fact proposal response value');
		expect(() =>
			parseFactProposalPageResponse({
				proposals: [{ ...proposal, evidence_status: 'supported' }],
				next_cursor: null,
			}),
		).toThrow('Invalid memory fact proposal response value');
		expect(() =>
			parseFactProposalPageResponse({
				proposals: [{ ...proposal, evidence_quote: 'x'.repeat(121) }],
				next_cursor: null,
			}),
		).toThrow('Invalid memory fact proposal response value');
		expect(() =>
			parseFactProposalPageResponse({
				proposals: [{ ...proposal, created_at: 'yesterday' }],
				next_cursor: null,
			}),
		).toThrow('Invalid memory fact proposal response value');
	});

	it('accepts only generated one-shot review results', () => {
		expect(parseFactProposalReviewResponse({ status: 'accepted', fact_written: true })).toEqual({
			status: 'accepted',
			fact_written: true,
		});
		expect(parseFactProposalReviewResponse({ status: 'already_reviewed' })).toEqual({
			status: 'already_reviewed',
		});
		expect(() => parseFactProposalReviewResponse({ status: 'accepted' })).toThrow(
			'Invalid memory fact proposal review response shape',
		);
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
			memory_kind: 'profile',
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
			valid_from: '2026-10-01T00:00:00.000Z',
			valid_to: null,
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
		expect(() =>
			parseMemoryRecallResponses(
				[
					{
						entity_id: 'fact-00000000000000000000000000000001',
						text: 'likes tea',
						score: 0.9,
						model: 'embed-v1',
					},
					{
						entity_id: 'fact-00000000000000000000000000000001',
						text: 'likes tea a lot',
						score: 0.8,
						model: 'embed-v1',
					},
				],
				'fact',
			),
		).toThrow('Duplicate memory recall identity');
	});
});
