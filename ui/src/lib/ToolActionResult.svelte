<script>
	let { data = {} } = $props();
</script>

{#if data.operation === 'result_injected'}
	<div class="tool-card-count">
		后台结果已回灌，正在继续{#if data.action_id} · {data.action_id}{/if}
	</div>
	{#if data.status}
		<div class="action-row">
			<span class="action-id">{data.action_id || '—'}</span>
			<span class="status-badge status-{data.status}">{data.status}</span>
		</div>
	{/if}
{:else if Array.isArray(data.actions)}
	<div class="tool-card-count">{data.actions.length} 个后台任务</div>
	{#if data.actions.length > 0}
		<div class="tool-card-list">
			{#each data.actions as action (action.action_id ?? action.job_id)}
				<div class="action-row">
					<span class="action-id">{action.action_id ?? action.job_id}</span>
					<span class="status-badge status-{action.status}">{action.status}</span>
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有后台任务</p>
	{/if}
{:else}
	<div class="action-row">
		<span class="action-id">{data.action_id ?? data.job_id}</span>
		<span class="status-badge status-{data.status}">{data.status}</span>
	</div>
	{#if data.exit_code != null}
		<div class="tool-card-meta">退出码 {data.exit_code}</div>
	{/if}
{/if}

<style>
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
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
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-empty {
		margin: 0;
	}
	.tool-card-meta {
		font-size: 11px;
		margin-top: var(--md-sys-space-2xs);
	}
	.action-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: 12px;
	}
	.action-id {
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.status-badge {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		padding: 1px 8px;
		border-radius: var(--md-sys-shape-full);
	}
	.status-completed {
		background: var(--md-sys-color-success);
		color: var(--md-sys-color-on-success-container);
	}
	.status-failed {
		background: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
	}
	.status-running {
		background: var(--md-sys-color-secondary);
		color: var(--md-sys-color-on-secondary);
	}
	.status-cancelled,
	.status-not_found,
	.status-idle {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
