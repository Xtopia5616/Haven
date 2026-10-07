<script lang="ts">
	import { scheduleModeLabel, toolRunTitle } from '$lib/toolRunTerminology.ts';
	import ToolResultList from '$lib/ToolResultList.svelte';

	interface Props {
		data?: {
			operation?: string;
			cancelled?: string | number | boolean;
			scheduled_tool_runs?: Array<{
				id: string;
				title?: string;
				body?: string;
				mode?: string;
				fires_at?: string;
			}>;
			id?: string;
			mode?: string;
			fires_at?: string;
			title?: string;
			body?: string;
		};
	}

	let { data = {} }: Props = $props();
	let operation = $derived(
		typeof data.operation === 'string'
			? data.operation.replace(/^schedule_/, '')
			: data.operation,
	);
</script>

{#if operation === 'cancel'}
	<div class="tool-run-row">
		<span class="scheduled-mode">已取消</span>
		{#if data.cancelled}<span class="tool-run-id">#{data.cancelled}</span>{/if}
	</div>
{:else if Array.isArray(data.scheduled_tool_runs)}
	<div class="tool-result-label">{data.scheduled_tool_runs.length} 条定时任务</div>
	{#if data.scheduled_tool_runs.length > 0}
		<ToolResultList items={data.scheduled_tool_runs}>
			{#snippet children(visibleToolRuns)}
				<div class="tool-result-scroll-area">
					{#each visibleToolRuns as toolRun (toolRun.id)}
						<div class="scheduled-row">
							<span class="scheduled-title"
								>{toolRunTitle({
									kind: 'scheduled',
									title: toolRun.title,
									body: toolRun.body,
								})}</span
							>
							<span class="scheduled-mode">{scheduleModeLabel(toolRun.mode)}</span>
							{#if toolRun.fires_at}<span class="scheduled-time"
									>{toolRun.fires_at}</span
								>{/if}
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有待触发的定时任务</p>
	{/if}
{:else if operation === 'set' || (data.id && data.mode)}
	<div class="tool-run-row">
		<span class="tool-run-id">#{data.id}</span>
		<span class="scheduled-mode">{scheduleModeLabel(data.mode)}</span>
	</div>
	{#if data.fires_at}
		<div class="tool-card-meta">触发时间 {data.fires_at}</div>
	{/if}
{:else}
	<div class="tool-card-meta">定时任务结果</div>
{/if}

<style>
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		margin-top: var(--md-sys-space-2xs);
	}
	.scheduled-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.scheduled-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.scheduled-title {
		flex: 1;
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.scheduled-mode {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.scheduled-time {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
