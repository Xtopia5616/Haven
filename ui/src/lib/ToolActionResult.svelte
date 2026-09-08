<script>
	import { actionStatusLabel } from '$lib/taskTerminology.ts';
	import JsonView from '$lib/JsonView.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolCardList from '$lib/ToolCardList.svelte';

	let { data = {} } = $props();

	/** @param {unknown} status */
	function statusTone(status) {
		const value = String(status ?? '').toLowerCase();
		if (value.includes('error') || value.includes('fail')) return 'error';
		if (value.includes('run') || value.includes('progress')) return 'info';
		if (value.includes('cancel') || value.includes('pause')) return 'warning';
		if (value.includes('complete') || value.includes('success') || value === 'done') return 'success';
		return 'neutral';
	}
</script>

{#if data.operation === 'result_injected'}
	<div class="tool-card-count">
		后台任务结果已回灌，正在继续{#if data.action_id} · {data.action_id}{/if}
	</div>
	{#if data.status}
		<div class="action-row">
			<span class="action-id">{data.action_id || '—'}</span>
			<StatusBadge label={actionStatusLabel(data.status)} tone={statusTone(data.status)} />
		</div>
	{/if}
{:else if data.operation === 'cancel'}
	<div class="action-row">
		<StatusBadge label={data.cancelled ? '已取消' : '未找到任务'} tone={data.cancelled ? 'success' : 'neutral'} />
		<span class="action-id">{data.action_id || '—'}</span>
	</div>
{:else if Array.isArray(data.actions)}
	<div class="tool-card-count">{data.actions.length} 个后台任务</div>
	{#if data.actions.length > 0}
		<ToolCardList>
			{#each data.actions as action (action.action_id ?? action.job_id)}
				<div class="action-row">
					<span class="action-id">{action.action_id ?? action.job_id}</span>
					<StatusBadge label={actionStatusLabel(action.status)} tone={statusTone(action.status)} />
				</div>
			{/each}
		</ToolCardList>
	{:else}
		<p class="tool-card-empty">没有后台任务</p>
	{/if}
{:else if data.action_id || data.job_id || data.status}
	<div class="action-row">
		<span class="action-id">{data.action_id ?? data.job_id}</span>
		<StatusBadge label={actionStatusLabel(data.status)} tone={statusTone(data.status)} />
	</div>
	{#if data.exit_code != null}
		<div class="tool-card-meta">退出码 {data.exit_code}</div>
	{/if}
{:else}
	<div class="tool-card-meta">后台任务操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-empty,
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-empty {
		margin: 0;
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		margin-top: var(--md-sys-space-2xs);
	}
	.action-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.action-id {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
