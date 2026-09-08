<script>
	import { scheduleModeLabel, taskTitle } from '$lib/taskTerminology.ts';

	let { data = {} } = $props();
</script>

{#if data.operation === 'cancel'}
	<div class="action-row">
		<span class="scheduled-mode">已取消</span>
		{#if data.cancelled}<span class="action-id">#{data.cancelled}</span>{/if}
	</div>
{:else if Array.isArray(data.scheduled_actions)}
	<div class="tool-card-count">{data.scheduled_actions.length} 条定时任务</div>
	{#if data.scheduled_actions.length > 0}
		<div class="tool-card-list">
			{#each data.scheduled_actions as action (action.id)}
				<div class="scheduled-row">
					<span class="scheduled-title">{taskTitle({ kind: 'scheduled', title: action.title, body: action.body })}</span>
					<span class="scheduled-mode">{scheduleModeLabel(action.mode)}</span>
					{#if action.fires_at}<span class="scheduled-time">{action.fires_at}</span>{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有待触发的定时任务</p>
	{/if}
{:else if data.operation === 'set' || (data.id && data.mode)}
	<div class="action-row">
		<span class="action-id">#{data.id}</span>
		<span class="scheduled-mode">{scheduleModeLabel(data.mode)}</span>
	</div>
	{#if data.fires_at}
		<div class="tool-card-meta">触发时间 {data.fires_at}</div>
	{/if}
{:else}
	<div class="tool-card-meta">定时任务结果</div>
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
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
	}
</style>
