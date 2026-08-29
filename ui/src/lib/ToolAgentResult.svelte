<script>
	let { data = {} } = $props();
</script>

{#if data.auto && typeof data.text === 'string'}
	<div class="tool-card-count">自动收到同伴消息（低信任）</div>
	<pre class="content-preview">{data.text}</pre>
{:else if Array.isArray(data.agents)}
	<div class="tool-card-count">{data.agents.length} 个同伴</div>
	{#if data.agents.length > 0}
		<div class="tool-card-list">
			{#each data.agents as agent (agent.name ?? agent.session_id ?? agent.agent)}
				<div class="action-row">
					<span class="action-id">{agent.title || agent.name || agent.session_id || agent.agent}</span>
					{#if agent.role}<span class="scheduled-mode">{agent.role}</span>{/if}
					{#if agent.status}<span class="status-badge status-{agent.status === 'online' ? 'completed' : 'cancelled'}">{agent.status}</span>{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有已注册同伴</p>
	{/if}
{:else if data.timed_out}
	<div class="action-row">
		<span class="status-badge status-failed">超时</span>
		{#if data.message_id}<span class="action-id">{data.message_id}</span>{/if}
	</div>
	<div class="tool-card-meta">等待同伴回复超时（{data.timeout_secs ?? '?'}s）</div>
{:else if data.session_id || data.agent}
	<div class="action-row">
		<span class="status-badge status-{data.ok === false ? 'failed' : 'completed'}">{data.ok === false ? '失败' : '已创建'}</span>
		<span class="action-id">{data.session_id || data.agent}</span>
	</div>
	{#if data.parent}<div class="tool-card-meta">父会话 {data.parent}</div>{/if}
	{#if data.role}<div class="tool-card-meta">角色 {data.role}</div>{/if}
	{#if data.queued}<div class="tool-card-meta">子会话已排队（运行中 {data.running_sessions ?? '?'}/{data.max_concurrent ?? '?'}）</div>{/if}
{:else if data.reply}
	<div class="tool-card-count">收到回复</div>
	<pre class="content-preview">{typeof data.reply === 'string' ? data.reply : JSON.stringify(data.reply, null, 2)}</pre>
{:else if typeof data.text === 'string' && data.text}
	<pre class="content-preview">{data.text}</pre>
{:else}
	<pre class="content-preview">{JSON.stringify(data, null, 2)}</pre>
{/if}

<style>
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-meta {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
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
	.scheduled-mode {
		flex: none;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
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
	.status-cancelled {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
