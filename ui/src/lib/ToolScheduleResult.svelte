<script lang="ts">
	import { scheduleModeLabel } from '$lib/toolRunTerminology.ts';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type { ScheduleMode } from './contracts/schedule.ts';
	import type { ToolScheduleResultOperation } from './toolResultPresentation.ts';

	interface Props {
		data?: {
			operation?: ToolScheduleResultOperation | null;
			schedule_id?: string | null;
			schedules?: Array<{
				schedule_id: string;
				title: string;
				body: string;
				mode: ScheduleMode;
				due_at?: string | null;
				next_due_at?: string | null;
			}>;
			mode?: ScheduleMode | null;
			fires_at?: string | null;
		};
	}

	let { data = {} }: Props = $props();
	let operation = $derived(data.operation);
</script>

{#if operation === 'cancel'}
	<div class="tool-run-row">
		<span class="scheduled-mode">已取消</span>
		{#if data.schedule_id}<span class="tool-run-id">#{data.schedule_id}</span>{/if}
	</div>
{:else if Array.isArray(data.schedules)}
	<div class="tool-result-label">{data.schedules.length} 条定时计划</div>
	{#if data.schedules.length > 0}
		<ToolResultList items={data.schedules}>
			{#snippet children(visibleSchedules)}
				<div class="tool-result-scroll-area">
					{#each visibleSchedules as schedule (schedule.schedule_id)}
						<div class="scheduled-row">
							<span class="scheduled-title">{schedule.title}</span>
							<span class="scheduled-mode">{scheduleModeLabel(schedule.mode)}</span>
							{#if schedule.next_due_at || schedule.due_at}
								<span class="scheduled-time">{schedule.next_due_at || schedule.due_at}</span>
							{/if}
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有待触发的定时任务</p>
	{/if}
{:else if operation === 'set' || (data.schedule_id && data.mode)}
	<div class="tool-run-row">
		<span class="tool-run-id">#{data.schedule_id}</span>
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
