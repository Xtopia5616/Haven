<script lang="ts">
	import { toolRunStatusLabel } from '$lib/toolRunTerminology.ts';
	import JsonView from '$lib/JsonView.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';

	type ToolRunSummary = { tool_run_id?: string; status?: string };

	interface Props {
		data?: {
			operation?: string;
			tool_run_id?: string;
			status?: string;
			cancelled?: boolean;
			tool_runs?: ToolRunSummary[];
			exit_code?: number | null;
		};
	}

	let { data = {} }: Props = $props();
	let operation = $derived(
		typeof data.operation === 'string'
			? data.operation.replace(/^tool_runs_/, '')
			: data.operation,
	);

	function statusTone(status: unknown): 'error' | 'info' | 'warning' | 'success' | 'neutral' {
		const value = String(status ?? '').toLowerCase();
		if (value.includes('error') || value.includes('fail')) return 'error';
		if (value.includes('run') || value.includes('progress')) return 'info';
		if (value.includes('cancel') || value.includes('pause')) return 'warning';
		if (value.includes('complete') || value.includes('success') || value === 'done')
			return 'success';
		return 'neutral';
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
	<div class="tool-run-row">
		<span class="tool-run-id">{data.tool_run_id || '—'}</span>
		<StatusBadge label={toolRunStatusLabel(data.status)} tone={statusTone(data.status)} />
	</div>
	{#if data.exit_code != null}
		<div class="tool-card-meta">退出码 {data.exit_code}</div>
	{/if}
{:else}
	<div class="tool-card-meta">后台任务操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
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
	.tool-run-id {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
