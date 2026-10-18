<script lang="ts">
	import { toolRunStatusLabel } from '$lib/toolRunTerminology.ts';
	import JsonView from '$lib/JsonView.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type { ToolRunStatus } from '$lib/contracts/toolRun.ts';
	import {
		type ToolRunResultStatus,
		type ToolRunsResultOperation,
	} from '$lib/toolResultPresentation.ts';

	type ToolRunSummary = { tool_run_id: string; status: ToolRunStatus };

	interface Props {
		data?: {
			operation?: ToolRunsResultOperation | null;
			tool_run_id?: string | null;
			status?: ToolRunResultStatus | null;
			cancelled?: boolean | null;
			tool_runs?: ToolRunSummary[];
			tool_name?: string | null;
			output?: string | null;
		};
	}

	let { data = {} }: Props = $props();
	let operation = $derived(data.operation);

	const statusTones: Record<
		ToolRunResultStatus,
		'error' | 'info' | 'warning' | 'success' | 'neutral'
	> = {
		running: 'info',
		completed: 'success',
		failed: 'error',
		cancelled: 'warning',
		not_found: 'neutral',
	};
	function statusTone(status: ToolRunResultStatus | null | undefined) {
		return status ? statusTones[status] : 'neutral';
	}
</script>

{#if operation === 'result_injected'}
	<div class="tool-result-label">
		后台任务结果已回灌，正在继续{#if data.tool_run_id}
			· {data.tool_run_id}{/if}
	</div>
	{#if data.status}
		<div class="tool-run-row">
			<span class="tool-run-id">{data.tool_run_id || '—'}</span>
			<StatusBadge label={toolRunStatusLabel(data.status)} tone={statusTone(data.status)} />
		</div>
	{/if}
{:else if operation === 'cancel'}
	<div class="tool-run-row">
		<StatusBadge
			label={data.cancelled ? '已取消' : '未找到任务'}
			tone={data.cancelled ? 'success' : 'neutral'}
		/>
		<span class="tool-run-id">{data.tool_run_id || '—'}</span>
	</div>
{:else if Array.isArray(data.tool_runs)}
	<div class="tool-result-label">{data.tool_runs.length} 个后台任务</div>
	{#if data.tool_runs.length > 0}
		<ToolResultList items={data.tool_runs}>
			{#snippet children(visibleToolRuns)}
				<div class="tool-result-scroll-area">
					{#each visibleToolRuns as toolRun (toolRun.tool_run_id)}
						<div class="tool-run-row">
							<span class="tool-run-id">{toolRun.tool_run_id}</span>
							<StatusBadge
								label={toolRunStatusLabel(toolRun.status)}
								tone={statusTone(toolRun.status)}
							/>
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有后台任务</p>
	{/if}
{:else if data.tool_run_id || data.status}
	<div class="tool-result-label">
		{data.status === 'running'
			? '后台任务运行中'
			: data.status === 'completed'
				? '后台任务已完成'
				: data.status === 'failed'
					? '后台任务失败'
					: data.status === 'cancelled'
						? '后台任务已取消'
						: '后台任务状态'}
	</div>
	<div class="tool-run-row">
		<span class="tool-run-id">{data.tool_run_id || '—'}</span>
		<StatusBadge label={toolRunStatusLabel(data.status)} tone={statusTone(data.status)} />
		{#if data.tool_name}<span class="tool-run-target">{data.tool_name}</span>{/if}
	</div>
	{#if data.output}<p class="tool-result-message">{data.output}</p>{/if}
{:else}
	<div class="tool-result-meta tool-result-meta--compact">后台任务操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.tool-run-id {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
