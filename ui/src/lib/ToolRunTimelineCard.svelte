<script lang="ts">
	import { projectToolRunCard } from '$lib/toolRunCardProjection.ts';
	import { toolRunStatusLabel, scheduleModeLabel, toolRunKindLabel } from '$lib/toolRunTerminology.ts';
	import ToolResultCard from '$lib/ToolResultCard.svelte';
	import type { ToolRunPayload } from '$lib/contracts/toolRun.ts';

	interface Props {
		toolRun?: ToolRunPayload;
		awaitingBackgroundResult?: boolean;
		awaitingBackgroundCount?: number;
		showTerminalOutput?: boolean;
	}

	let {
		toolRun = undefined,
		awaitingBackgroundResult = false,
		awaitingBackgroundCount = 0,
		showTerminalOutput = true,
	}: Props = $props();

	function formatDate(value?: string): string {
		if (!value) return '';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return '';
		return new Intl.DateTimeFormat('zh-CN', {
			month: 'numeric',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
		}).format(date);
	}

	function toolRunDuration(value: ToolRunPayload): string {
		const start = new Date(value.startedAt ?? '').getTime();
		if (Number.isNaN(start)) return '';
		const end =
			value.status === 'running'
				? Date.now()
				: new Date(value.finishedAt || value.startedAt || '').getTime();
		if (Number.isNaN(end)) return '';
		const seconds = Math.max(0, Math.floor((end - start) / 1000));
		if (seconds < 60) return `${seconds} 秒`;
		const minutes = Math.floor(seconds / 60);
		return `${minutes} 分 ${seconds % 60} 秒`;
	}

	const projection = $derived(
		toolRun
			? projectToolRunCard(toolRun, {
					toolRunStatusLabel,
					sessionTitleFor: () => '',
					toolRunDuration,
					scheduledToolRunCountdown: (dueAt) => formatDate(dueAt),
				})
			: null,
	);
	const triggerTime = $derived.by(() => {
		if (!toolRun) return '';
		if (toolRun.kind === 'scheduled') {
			if (toolRun.status === 'running' && toolRun.startedAt) {
				return `已触发 ${formatDate(toolRun.startedAt)}`;
			}
			return toolRun.dueAt ? `计划触发 ${formatDate(toolRun.dueAt)}` : '触发时间未设置';
		}
		return toolRun.startedAt ? `开始于 ${formatDate(toolRun.startedAt)}` : '';
	});
	const summary = $derived(
		toolRun?.kind === 'scheduled' ? toolRun.body || '' : '',
	);
	const backgroundResult = $derived(
		JSON.stringify({
			background: true,
			tool_run_id: toolRun?.id,
			status: toolRun?.status ?? 'running',
			output: showTerminalOutput
				? toolRun?.output || toolRun?.preview || toolRun?.error || ''
				: '',
			...(toolRun?.exitCode != null ? { exit_code: toolRun.exitCode } : {}),
		}),
	);
</script>

{#if toolRun?.kind !== 'scheduled'}
	<ToolResultCard
		type="tool"
		toolName="shell"
		outcome={toolRun?.status ?? 'running'}
		content={backgroundResult}
		toolArgs={toolRun?.command ? { command: toolRun.command } : null}
		messageId={toolRun?.sourceStepId ?? toolRun?.id ?? 'background-tool-run-wait'}
		toolRunId={showTerminalOutput ? toolRun?.id ?? null : null}
		toolRunData={showTerminalOutput ? toolRun ?? null : null}
		toolRunOutputHidden={!!toolRun && !showTerminalOutput}
		{awaitingBackgroundResult}
		{awaitingBackgroundCount}
	/>
{:else}
<article
	class="tool-run-timeline-card"
	data-kind="scheduled"
	data-tone={projection?.tone ?? 'waiting'}
	aria-label={`${toolRunKindLabel('scheduled')}：${projection?.title ?? ''}，${projection?.statusLabel ?? ''}`}
>
	<div class="tool-run-header">
		<span class="tool-run-kind">{toolRunKindLabel('scheduled')}</span>
		{#if projection?.statusLabel}
			<span class="md-badge" data-variant={projection.tone}>{projection.statusLabel}</span>
		{/if}
		{#if projection?.timing && toolRun?.status === 'running'}
			<span class="tool-run-timing">{projection.timing}</span>
		{/if}
	</div>

	<div class="tool-run-title-row">
		<strong>{projection?.title ?? '定时任务'}</strong>
		{#if triggerTime}<span class="tool-run-trigger-time">{triggerTime}</span>{/if}
	</div>

	{#if summary}
		<p class="tool-run-summary">{summary}</p>
	{/if}

	<div class="scheduled-details">
		<span>{scheduleModeLabel(toolRun.mode)}</span>
	</div>
</article>
{/if}

<style>
	.tool-run-timeline-card {
		width: var(--md-sys-chat-surface-width);
		max-width: var(--md-sys-chat-surface-width);
		margin-right: auto;
		box-sizing: border-box;
		display: grid;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-inline-start: 3px solid var(--md-sys-color-tertiary);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		box-shadow: var(--md-sys-elevation-1);
	}
	.tool-run-timeline-card[data-tone='running'],
	.tool-run-timeline-card[data-tone='scheduled'],
	.tool-run-timeline-card[data-tone='waiting'] {
		border-inline-start-color: var(--md-sys-color-tertiary);
	}
	.tool-run-timeline-card[data-tone='success'] {
		border-inline-start-color: var(--md-sys-color-success);
	}
	.tool-run-timeline-card[data-tone='error'] {
		border-inline-start-color: var(--md-sys-color-error);
	}
	.tool-run-timeline-card[data-tone='neutral'] {
		border-inline-start-color: var(--md-sys-color-outline);
	}
	.tool-run-header,
	.tool-run-title-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.tool-run-kind {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-weight: 700;
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-run-timing,
	.tool-run-trigger-time {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-run-timing {
		margin-inline-start: auto;
	}
	.tool-run-title-row strong {
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
		font-weight: 650;
	}
	.tool-run-trigger-time {
		margin-inline-start: auto;
	}
	.tool-run-summary,
	.scheduled-details {
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.scheduled-details {
		display: grid;
		gap: var(--md-sys-space-2xs);
	}
	@media (max-width: 600px) {
		.tool-run-timeline-card {
			padding: var(--md-sys-space-md);
		}
		.tool-run-trigger-time {
			margin-inline-start: 0;
		}
	}
</style>
