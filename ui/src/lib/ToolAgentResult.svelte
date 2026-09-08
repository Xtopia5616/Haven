<script>
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolCardList from '$lib/ToolCardList.svelte';
	let { data = {} } = $props();
</script>

{#if data.auto && typeof data.text === 'string'}
	<div class="tool-card-count">自动收到同伴消息（低信任）</div>
	<pre class="content-preview">{data.text}</pre>
{:else if Array.isArray(data.agents)}
	<div class="tool-card-count">{data.agents.length} 个同伴</div>
	{#if data.agents.length > 0}
		<ToolCardList>
			{#each data.agents as agent (agent.name ?? agent.session_id ?? agent.agent)}
				<div class="action-row">
					<span class="action-id">{agent.title || agent.name || agent.session_id || agent.agent}</span>
					{#if agent.role}<span class="scheduled-mode">{agent.role}</span>{/if}
					{#if agent.status}<StatusBadge label={agent.status} tone={agent.status === 'online' ? 'success' : 'neutral'} />{/if}
				</div>
			{/each}
		</ToolCardList>
	{:else}
		<p class="tool-card-empty">没有已注册同伴</p>
	{/if}
{:else if data.timed_out}
	<div class="action-row">
		<StatusBadge label="超时" tone="error" />
		{#if data.message_id}<span class="action-id">{data.message_id}</span>{/if}
	</div>
	<div class="tool-card-meta">等待同伴回复超时（{data.timeout_secs ?? '?'}s）</div>
{:else if data.session_id || data.agent}
	<div class="action-row">
		<StatusBadge label={data.ok === false ? '失败' : '已创建'} tone={data.ok === false ? 'error' : 'success'} />
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
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
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
	.scheduled-mode {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
