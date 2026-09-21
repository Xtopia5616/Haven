<script>
	import { untrack } from 'svelte';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import Icon from '$lib/Icon.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MediaPlanCard from '$lib/MediaPlanCard.svelte';
	import { hasToolPreambleBefore } from '$lib/toolIntent.ts';
	import { toolDisplayName } from '$lib/toolIdentity.ts';

	let {
		entries = [],
		streaming = false,
		toolCount = 0,
		stepCount = 0,
		allMessages = [],
		onContextMenu = () => {},
		onAskSelectionChange = () => {},
		onIgnore = () => {},
		onAskSubmit = () => {},
		mediaPlans = [],
	} = $props();

	// A work process is visible while it is active, then becomes a compact
	// summary. Manual expansion after completion is preserved across updates.
	let open = $state(untrack(() => streaming));
	let lastStreaming = untrack(() => streaming);
	$effect.pre(() => {
		if (streaming === lastStreaming) return;
		open = streaming;
		lastStreaming = streaming;
	});

	let currentEntry = $derived.by(() => {
		for (let index = entries.length - 1; index >= 0; index -= 1) {
			if (entries[index].message.streaming) return entries[index].message;
		}
		return entries[entries.length - 1]?.message ?? null;
	});

	let summary = $derived.by(() => {
		if (!currentEntry) return '工作过程';
		if (streaming) {
			if (currentEntry.type === 'tool') {
				return `正在执行 ${toolDisplayName(String(currentEntry.toolName || '工具'))}`;
			}
			if (currentEntry.type === 'reasoning') return '正在思考';
			return '正在整理下一步';
		}
		return toolCount > 0 ? `已完成 ${toolCount} 个操作` : '已完成工作过程';
	});

	let meta = $derived(
		streaming
			? `${stepCount || entries.length} 个步骤`
			: `${stepCount || entries.length} 个步骤 · 点击查看详情`,
	);
	let activityStepNumbers = $derived(
		new Set(entries.map(({ message }) => message.stepNumber).filter((step) => step != null)),
	);
	let visibleMediaPlans = $derived(
		mediaPlans.filter((plan) => activityStepNumbers.has(plan.stepNumber)),
	);
</script>

<section
	class="activity-group"
	class:is-streaming={streaming}
	data-surface="outlined"
	aria-label="Agent 工作过程"
>
	<!-- Keep child disclosure components mounted. The outer summary is a
	     second, independent disclosure; lazy-unmounting it reset the manual
	     open/closed state of the nested reasoning and tool-result cards. -->
	<MaterialCollapsible bind:open>
		{#snippet header()}
			<span class="activity-status" aria-hidden="true">
				{#if streaming}
					<span class="activity-pulse"></span>
				{:else}
					<Icon name="check" size={13} strokeWidth={2.5} />
				{/if}
			</span>
			<span class="activity-summary">{summary}</span>
			<span class="activity-meta">{meta}</span>
		{/snippet}

		{#if visibleMediaPlans.length > 0}
			<div class="media-plan-items">
				{#each visibleMediaPlans as plan (`${plan.stepNumber}:${plan.runId}:${plan.role}`)}
					<MediaPlanCard {plan} />
				{/each}
			</div>
		{/if}

		<div class="activity-items">
			{#each entries as entry (entry.message.id)}
				{@const msg = entry.message}
				{@const showFallbackIntent =
					msg.type === 'tool' &&
					(msg.showFallbackIntent ?? !hasToolPreambleBefore(allMessages, entry.index))}
				<ChatBubble
					role={String(msg.role || 'assistant')}
					content={String(msg.content || '')}
					type={msg.type || null}
					voice={!!msg.voice}
					time={msg.time || null}
					streaming={!!msg.streaming}
					toolName={String(msg.toolName || '')}
					outcome={msg.outcome || null}
					renderer={msg.renderer || null}
					result={msg.result}
					messageId={msg.id}
					stepNumber={msg.stepNumber ?? null}
					toolArgs={msg.toolArgs ?? null}
					attachments={msg.attachments || []}
					{showFallbackIntent}
					options={msg.options || []}
					awaiting={!!msg.awaiting}
					received={!!msg.received}
					resolved={msg.resolved || null}
					actionId={msg.actionId || null}
					compact
					{onContextMenu}
					{onAskSelectionChange}
					{onIgnore}
					{onAskSubmit}
				/>
			{/each}
		</div>
	</MaterialCollapsible>
</section>

<style>
	.activity-group {
		width: var(--md-sys-chat-surface-width);
		max-width: var(--md-sys-chat-surface-width);
		margin-right: auto;
		box-sizing: border-box;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 18%, var(--md-sys-color-outline-variant));
		border-radius: var(--md-sys-shape-large);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 18%,
			var(--md-sys-color-surface-container-low)
		);
		color: var(--md-sys-color-on-surface);
		box-shadow: var(--md-sys-elevation-1);
	}
	.activity-group.is-streaming {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-primary) 30%,
			var(--md-sys-color-outline-variant)
		);
	}
	/* The message list uses intrinsic-size virtualization for long sessions.
	 * An open group is actively being measured by stableReveal, so its
	 * children must expose their actual height during that measurement. */
	.activity-group :global(.bubble) {
		content-visibility: visible;
		contain-intrinsic-size: none;
	}
	.activity-group :global(.md-collapsible-header) {
		min-height: 28px;
		padding: var(--md-sys-space-2xs) 0;
	}
	.activity-group :global(.md-collapsible-caret) {
		margin-inline-start: var(--md-sys-space-xs);
	}
	.activity-status {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 18px;
		flex: none;
		border-radius: var(--md-sys-shape-full);
		color: var(--md-sys-color-success);
	}
	.activity-group.is-streaming .activity-status {
		color: var(--md-sys-color-primary);
	}
	.activity-pulse {
		width: 8px;
		height: 8px;
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		animation: activity-pulse 1.2s ease-in-out infinite;
	}
	.activity-summary {
		min-width: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.activity-meta {
		margin-left: auto;
		flex: none;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.activity-items {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-sm) 0 var(--md-sys-space-xs);
	}
	.media-plan-items {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding-top: var(--md-sys-space-sm);
	}
	@keyframes activity-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.9);
		}
		50% {
			opacity: 1;
			transform: scale(1);
		}
	}
	@media (max-width: 640px) {
		.activity-group {
			width: 100%;
			max-width: 100%;
			padding-inline: var(--md-sys-space-2xs);
		}
		.activity-meta {
			display: none;
		}
	}
</style>
