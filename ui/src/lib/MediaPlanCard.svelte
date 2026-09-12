<script lang="ts">
	import type { AgentMediaPlanPayload } from '$lib/contracts/agent.ts';
	import {
		mediaPlanNoticeLabel,
		mediaPlanProjectionLabel,
		mediaPlanStrategyLabel,
	} from '$lib/mediaPlanPresentation.ts';

	let { plan }: { plan: AgentMediaPlanPayload } = $props();
	let noticeLabels = $derived(
		[...new Set(plan.notices.map((notice) => mediaPlanNoticeLabel(notice.code)))],
	);
	let title = $derived(plan.notices.length > 0 ? '附件已调整' : '附件表示选择');
</script>

<section class="media-plan-card" aria-label="附件表示计划">
	<div class="media-plan-head">
		<span class="media-plan-title">{title}</span>
		<span class="media-plan-step">第 {plan.stepNumber} 步</span>
	</div>
	<p class="media-plan-meta">
		{plan.role} · 策略：{mediaPlanStrategyLabel(plan.strategy)}
	</p>
	{#if plan.projections.length > 0}
		<ul class="media-plan-list">
			{#each plan.projections as projection}
				<li>{mediaPlanProjectionLabel(projection)}</li>
			{/each}
		</ul>
	{:else}
		<p class="media-plan-empty">本次没有发送兼容的附件表示。</p>
	{/if}
	{#if noticeLabels.length > 0}
		<p class="media-plan-reasons">原因：{noticeLabels.join('；')}</p>
	{/if}
</section>

<style>
	.media-plan-card {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-tertiary) 28%, var(--md-sys-color-outline-variant));
		border-radius: var(--md-sys-shape-medium);
		background: color-mix(
			in srgb,
			var(--md-sys-color-tertiary-container) 22%,
			var(--md-sys-color-surface-container-lowest)
		);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.media-plan-head {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-sm);
	}
	.media-plan-title {
		font-weight: 650;
		color: var(--md-sys-color-on-surface);
	}
	.media-plan-step,
	.media-plan-meta {
		color: var(--md-sys-color-on-surface-variant);
	}
	.media-plan-step {
		margin-left: auto;
		font-size: var(--md-sys-typescale-label-small-size);
	}
	.media-plan-meta,
	.media-plan-empty,
	.media-plan-reasons {
		margin: var(--md-sys-space-2xs) 0 0;
	}
	.media-plan-list {
		margin: var(--md-sys-space-xs) 0 0;
		padding-left: var(--md-sys-space-lg);
		color: var(--md-sys-color-on-surface);
	}
	.media-plan-reasons {
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
