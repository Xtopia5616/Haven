<script lang="ts">
	import { scheduleModeLabel, toolRunTitle } from '$lib/toolRunTerminology.ts';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import {
		normalizeToolScheduleOperation,
		type ToolScheduleMode,
		type ToolScheduleResultOperation,
	} from './toolResultPresentation.ts';

	interface Props {
		data?: {
			operation?: ToolScheduleResultOperation | null;
			tool_run_id?: string | null;
			scheduled_tool_runs?: Array<{
				tool_run_id: string;
				title: string;
				body: string;
				mode: ToolScheduleMode;
				due_at: string;
			}>;
			mode?: ToolScheduleMode | null;
			fires_at?: string | null;
		};
	}

	let { data = {} }: Props = $props();
	let operation = $derived(normalizeToolScheduleOperation(data.operation));
</script>

{#if operation === 'cancel'}
	<div class="tool-run-row">
		<span class="scheduled-mode">已取消</span>
		{#if data.tool_run_id}<span class="tool-run-id">#{data.tool_run_id}</span>{/if}
	</div>
{:else if Array.isArray(data.scheduled_tool_runs)}
	<div class="tool-result-label">{data.scheduled_tool_runs.length} 条定时任务</div>
	{#if data.scheduled_tool_runs.length > 0}
		<ToolResultList items={data.scheduled_tool_runs}>
			{#snippet children(visibleToolRuns)}
				<div class="tool-result-scroll-area">
					{#each visibleToolRuns as toolRun (toolRun.tool_run_id)}
						<div class="scheduled-row">
							<span class="scheduled-title"
								>{toolRunTitle({
									kind: 'scheduled',
									title: toolRun.title,
									body: toolRun.body,
								})}</span
							>
							<span class="scheduled-mode">{scheduleModeLabel(toolRun.mode)}</span>
							{#if toolRun.due_at}<span class="scheduled-time">{toolRun.due_at}</span
								>{/if}
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有待触发的定时任务</p>
	{/if}
{:else if operation === 'set' || (data.tool_run_id && data.mode)}
	<div class="tool-run-row">
		<span class="tool-run-id">#{data.tool_run_id}</span>
		<span class="scheduled-mode">{scheduleModeLabel(data.mode)}</span>
	</div>
	{#if data.fires_at}
		<div class="tool-result-meta tool-result-meta--compact">触发时间 {data.fires_at}</div>
	{/if}
{:else}
	<div class="tool-result-meta tool-result-meta--compact">定时任务结果</div>
{/if}

<style>
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
