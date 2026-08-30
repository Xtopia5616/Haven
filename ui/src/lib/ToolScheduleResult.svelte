<script>
	let { data = {} } = $props();
</script>

{#if Array.isArray(data.scheduled_actions)}
	<div class="tool-card-count">{data.scheduled_actions.length} 条定时任务</div>
	{#if data.scheduled_actions.length > 0}
		<div class="tool-card-list">
			{#each data.scheduled_actions as reminder (reminder.id)}
				<div class="scheduled-row">
					<span class="scheduled-title">{reminder.title || reminder.body}</span>
					{#if reminder.mode}<span class="scheduled-mode">{reminder.mode}</span>{/if}
					{#if reminder.fires_at}<span class="scheduled-time">{reminder.fires_at}</span>{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有待触发的定时任务</p>
	{/if}
{:else}
	<div class="action-row">
		<span class="action-id">#{data.id}</span>
		<span class="scheduled-mode">{data.mode}</span>
	</div>
	{#if data.fires_at}
		<div class="tool-card-meta">触发时间 {data.fires_at}</div>
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
	.scheduled-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: 12px;
	}
	.scheduled-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.scheduled-title {
		flex: 1;
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.scheduled-mode {
		flex: none;
		font-size: 10px;
		font-weight: 600;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.scheduled-time {
		flex: none;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
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
	}
</style>
