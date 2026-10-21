<script lang="ts">
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import type { FactProposal, FactProposalDecision } from '$lib/contracts/memory.ts';

	interface Props {
		proposals: FactProposal[];
		loaded: boolean;
		hasMore: boolean;
		loadingMore: boolean;
		busyProposalId: string | null;
		onLoadMore: () => void;
		onReview: (proposalId: string, decision: FactProposalDecision) => void;
	}

	let {
		proposals,
		loaded,
		hasMore,
		loadingMore,
		busyProposalId,
		onLoadMore,
		onReview,
	}: Props = $props();

	function formatDate(value: string) {
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return value;
		return new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' }).format(date);
	}

	function confidenceLabel(confidence: number) {
		return `置信度 ${Math.round(Math.max(0, Math.min(1, confidence)) * 100)}%`;
	}

	function evidenceLabel(status: FactProposal['evidence_status']) {
		switch (status) {
			case 'uncertain':
				return '证据不明确';
			case 'contradicted':
				return '原话存在冲突';
			case 'unverified':
				return '未完成核验';
			case 'supported':
				return '原话支持';
		}
	}
</script>

<section class="proposal-review" aria-label="待审核的推断事实">
	<header class="proposal-review__header">
		<div>
			<h2>待审核的推断事实</h2>
			<p>系统会先核对用户原话；证据不明确或存在冲突的事实需要你审核。</p>
		</div>
		<MaterialBadge text={`${proposals.length}${hasMore ? '+' : ''} 条待审核`} />
	</header>

	{#if proposals.length > 0}
		<div class="proposal-review__list" role="list" aria-label="推断事实提案列表">
			{#each proposals as proposal (proposal.proposalId)}
				<article class="proposal-card" role="listitem">
					<div class="proposal-card__content">
						<div class="proposal-card__heading">
							<strong>{proposal.predicate}</strong>
							<MaterialBadge text={evidenceLabel(proposal.evidence_status)} />
							<MaterialBadge text={confidenceLabel(proposal.confidence)} />
						</div>
						<p class="proposal-card__fact">
							{proposal.subject === 'user' ? '你' : proposal.subject} · {proposal.object}
						</p>
						{#if proposal.evidence_quote || proposal.sourceRef?.snippet}
							<blockquote>“{proposal.evidence_quote ?? proposal.sourceRef?.snippet}”</blockquote>
						{/if}
						<span class="proposal-card__time">抽取于 {formatDate(proposal.created_at)}</span>
					</div>
					<div class="proposal-card__actions">
						<MaterialButton
							variant="text"
							label="拒绝"
							disabled={busyProposalId !== null}
							onclick={() => onReview(proposal.proposalId, 'reject')}
						/>
						<MaterialButton
							variant="filled"
							label={busyProposalId === proposal.proposalId ? '处理中…' : '接受'}
							ariaBusy={busyProposalId === proposal.proposalId}
							disabled={busyProposalId !== null}
							onclick={() => onReview(proposal.proposalId, 'accept')}
						/>
					</div>
				</article>
			{/each}
		</div>
	{:else if loaded}
		<AsyncState layout="compact" title="没有待审核事实" message="新的模型推断会先显示在这里。" />
	{:else}
		<AsyncState state="loading" layout="compact" title="正在读取审核队列" />
	{/if}

	{#if hasMore}
		<div class="proposal-review__footer">
			<MaterialButton
				variant="outlined"
				label={loadingMore ? '正在加载…' : '加载更多提案'}
				ariaBusy={loadingMore}
				disabled={loadingMore}
				onclick={onLoadMore}
			/>
		</div>
	{/if}
</section>

<style>
	.proposal-review {
		display: grid;
		gap: var(--md-sys-space-md);
		margin-block: var(--md-sys-space-lg);
		padding: var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-low);
	}
	.proposal-review__header,
	.proposal-card,
	.proposal-card__heading,
	.proposal-card__actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.proposal-review__header h2,
	.proposal-review__header p,
	.proposal-card__fact {
		margin: 0;
	}
	.proposal-review__header h2 {
		font-size: 1rem;
	}
	.proposal-review__header p,
	.proposal-card__time {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.875rem;
	}
	.proposal-review__list {
		display: grid;
		gap: var(--md-sys-space-sm);
	}
	.proposal-card {
		align-items: flex-start;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface);
	}
	.proposal-card__content {
		min-width: 0;
		display: grid;
		gap: var(--md-sys-space-xs);
	}
	.proposal-card__heading {
		justify-content: flex-start;
		flex-wrap: wrap;
	}
	.proposal-card blockquote {
		margin: 0;
		padding-inline-start: var(--md-sys-space-sm);
		border-inline-start: 2px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
	}
	.proposal-card__actions {
		flex-shrink: 0;
	}
	.proposal-review__footer {
		display: flex;
		justify-content: center;
	}
	@media (max-width: 680px) {
		.proposal-review__header,
		.proposal-card {
			align-items: stretch;
			flex-direction: column;
		}
		.proposal-card__actions {
			justify-content: flex-end;
		}
	}
</style>
