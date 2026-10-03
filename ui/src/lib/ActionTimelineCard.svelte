<script lang="ts">
	import { projectActionCard } from '$lib/actionCardProjection.ts';
	import { actionStatusLabel, scheduleModeLabel, taskKindLabel } from '$lib/taskTerminology.ts';
	import type { ActionPayload } from '$lib/contracts/action.ts';

	interface Props {
		action?: ActionPayload;
		awaitingBackgroundResult?: boolean;
		awaitingBackgroundCount?: number;
		showTerminalOutput?: boolean;
	}

	let {
		action = undefined,
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

	function actionDuration(value: ActionPayload): string {
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
		action
			? projectActionCard(action, {
					actionStatusLabel,
					sessionTitleFor: () => '',
					actionDuration,
					scheduledActionCountdown: (dueAt) => formatDate(dueAt),
				})
			: null,
	);
	const triggerTime = $derived.by(() => {
		if (!action) return '';
		if (action.kind === 'scheduled') {
			if (action.status === 'running' && action.startedAt) {
				return `已触发 ${formatDate(action.startedAt)}`;
			}
			return action.dueAt ? `计划触发 ${formatDate(action.dueAt)}` : '触发时间未设置';
		}
		return action.startedAt ? `开始于 ${formatDate(action.startedAt)}` : '';
	});
	const output = $derived(
		action?.kind === 'background' && showTerminalOutput
			? action.output || action.error || ''
			: '',
	);
	const summary = $derived(
		action?.kind === 'background'
			? action.command || action.errorReason || '后台任务正在执行'
			: action?.kind === 'scheduled'
				? action.body || ''
				: '',
	);
</script>

<article
	class="action-timeline-card"
	data-kind={action?.kind ?? 'background'}
	data-tone={projection?.tone ?? 'waiting'}
	aria-label={projection
		? `${taskKindLabel(projection.kind)}：${projection.title}，${projection.statusLabel}`
		: '后台任务等待结果'}
>
	<div class="action-header">
		<span class="action-kind">{taskKindLabel(action?.kind ?? 'background')}</span>
		{#if projection?.statusLabel}
			<span class="md-badge" data-variant={projection.tone}>{projection.statusLabel}</span>
		{:else if !action}
			<span class="md-badge" data-variant="waiting">等待结果</span>
		{/if}
		{#if projection?.timing && (action?.kind === 'background' || action?.status === 'running')}
			<span class="action-timing">{projection.timing}</span>
		{/if}
	</div>

	<div class="action-title-row">
		<strong>{projection?.title ?? '等待后台任务结果'}</strong>
		{#if triggerTime}<span class="action-trigger-time">{triggerTime}</span>{/if}
	</div>

	{#if summary}
		<p class="action-summary">{summary}</p>
	{/if}

	{#if action?.kind === 'scheduled'}
		<div class="scheduled-details">
			<span>{scheduleModeLabel(action.mode)}</span>
		</div>
	{/if}

	{#if action?.kind === 'background' && action.status === 'running' && action.preview}
		<pre class="action-preview">{action.preview}</pre>
	{/if}

	{#if action?.kind === 'background' && output}
		<details class="action-output">
			<summary>{action.status === 'failed' ? '查看失败输出' : '查看任务输出'}</summary>
			<pre>{output}</pre>
		</details>
	{/if}

	{#if awaitingBackgroundResult}
		<p class="action-wait-note" role="status">
			等待{awaitingBackgroundCount > 1
				? ` ${awaitingBackgroundCount} 项`
				: ''}后台任务结果，完成后将自动继续
		</p>
	{/if}
</article>

<style>
	.action-timeline-card {
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
	.action-timeline-card[data-tone='running'],
	.action-timeline-card[data-tone='scheduled'],
	.action-timeline-card[data-tone='waiting'] {
		border-inline-start-color: var(--md-sys-color-tertiary);
	}
	.action-timeline-card[data-tone='success'] {
		border-inline-start-color: var(--md-sys-color-success);
	}
	.action-timeline-card[data-tone='error'] {
		border-inline-start-color: var(--md-sys-color-error);
	}
	.action-timeline-card[data-tone='neutral'] {
		border-inline-start-color: var(--md-sys-color-outline);
	}
	.action-header,
	.action-title-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.action-kind {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-weight: 700;
		color: var(--md-sys-color-on-surface-variant);
	}
	.action-timing,
	.action-trigger-time {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.action-timing {
		margin-inline-start: auto;
	}
	.action-title-row strong {
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
		font-weight: 650;
	}
	.action-trigger-time {
		margin-inline-start: auto;
	}
	.action-summary,
	.scheduled-details,
	.action-wait-note {
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
	.action-preview,
	.action-output pre {
		margin: 0;
		max-height: 220px;
		overflow: auto;
		padding: var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container);
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.action-output summary {
		width: fit-content;
		cursor: pointer;
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.action-wait-note {
		padding-block-start: var(--md-sys-space-xs);
		border-block-start: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface);
		font-weight: 600;
	}
	@media (max-width: 600px) {
		.action-timeline-card {
			padding: var(--md-sys-space-md);
		}
		.action-trigger-time {
			margin-inline-start: 0;
		}
	}
</style>
