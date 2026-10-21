import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import PendingFactProposals from './PendingFactProposals.svelte';
import type { FactProposal } from '$lib/contracts/memory.ts';

const proposal: FactProposal = {
	proposalId: 'fprop-00000000000000000000000000000001',
	subject: 'user',
	predicate: 'likes',
	object: 'tea',
	confidence: 0.8,
	tags: ['preference'],
	durability: 0.5,
	created_at: '2026-10-12T08:00:00.000Z',
	evidence_status: 'unverified',
	evidence_quote: null,
	sourceRef: {
		messageId: 'msg-00000000000000000000000000000002',
		snippet: 'I like tea',
	},
};

describe('PendingFactProposals', () => {
	it('shows provenance and calls the matching one-shot review action', async () => {
		const onReview = vi.fn();
		render(PendingFactProposals, {
			proposals: [proposal],
			loaded: true,
			hasMore: false,
			loadingMore: false,
			busyProposalId: null,
			onLoadMore: vi.fn(),
			onReview,
		});

		expect(screen.getByText('likes', { selector: 'strong' })).toBeTruthy();
		expect(screen.getByText('未完成核验')).toBeTruthy();
		expect(screen.getByText('你 · tea')).toBeTruthy();
		expect(screen.getByText('“I like tea”')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '接受' }));
		expect(onReview).toHaveBeenCalledWith(proposal.proposalId, 'accept');
		await fireEvent.click(screen.getByRole('button', { name: '拒绝' }));
		expect(onReview).toHaveBeenCalledWith(proposal.proposalId, 'reject');
	});

	it('supports bounded pagination and a clean empty state', async () => {
		const onLoadMore = vi.fn();
		const { rerender } = render(PendingFactProposals, {
			proposals: [],
			loaded: true,
			hasMore: true,
			loadingMore: false,
			busyProposalId: null,
			onLoadMore,
			onReview: vi.fn(),
		});
		expect(screen.getByRole('heading', { name: '没有待审核事实' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '加载更多提案' }));
		expect(onLoadMore).toHaveBeenCalledTimes(1);

		await rerender({
			proposals: [proposal],
			loaded: true,
			hasMore: false,
			loadingMore: false,
			busyProposalId: proposal.proposalId,
			onLoadMore,
			onReview: vi.fn(),
		});
		expect((screen.getByRole('button', { name: '处理中…' }) as HTMLButtonElement).disabled).toBe(
			true,
		);
	});
});
