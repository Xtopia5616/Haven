<script lang="ts">
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type { ToolAgentPresenceStatus } from './toolResultPresentation.ts';

	interface Props {
		data?: {
			auto?: boolean;
			text?: string;
			agents?: Array<{
				name: string;
				title?: string | null;
				role?: string | null;
				status: ToolAgentPresenceStatus;
			}>;
			timed_out?: boolean;
			message_id?: string;
			timeout_secs?: number;
			session_id?: string;
			ok?: boolean;
			parent?: string;
			role?: string;
			queued?: boolean;
			running_sessions?: number;
			max_concurrent?: number;
			reply?: unknown;
		};
	}

	let { data = {} }: Props = $props();
</script>

{#if data.auto && typeof data.text === 'string'}
	<div class="tool-result-label">自动收到同伴消息（低信任）</div>
	<pre class="tool-result-preview">{data.text}</pre>
{:else if Array.isArray(data.agents)}
	<div class="tool-result-label">{data.agents.length} 个同伴</div>
	{#if data.agents.length > 0}
		<ToolResultList items={data.agents}>
			{#snippet children(visibleAgents)}
				<div class="tool-result-scroll-area">
					{#each visibleAgents as agent (agent.name)}
						<div class="tool-result-status-row">
							<span class="action-id">{agent.title || agent.name}</span>
							{#if agent.role}<span class="scheduled-mode">{agent.role}</span>{/if}
							{#if agent.status}<StatusBadge
									label={agent.status}
									tone={agent.status === 'online' ? 'success' : 'neutral'}
								/>{/if}
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有已注册同伴</p>
	{/if}
{:else if data.timed_out}
	<div class="tool-result-status-row">
		<StatusBadge label="超时" tone="error" />
		{#if data.message_id}<span class="action-id">{data.message_id}</span>{/if}
	</div>
	<div class="tool-result-meta tool-result-meta--compact">
		等待同伴回复超时（{data.timeout_secs ?? '?'}s）
	</div>
{:else if data.session_id}
	<div class="tool-result-status-row">
		<StatusBadge
			label={data.ok === false ? '失败' : '已创建'}
			tone={data.ok === false ? 'error' : 'success'}
		/>
		<span class="action-id">{data.session_id}</span>
	</div>
	{#if data.parent}<div class="tool-result-meta tool-result-meta--compact">
			父会话 {data.parent}
		</div>{/if}
	{#if data.role}<div class="tool-result-meta tool-result-meta--compact">
			角色 {data.role}
		</div>{/if}
	{#if data.queued}<div class="tool-result-meta tool-result-meta--compact">
			子会话已排队（运行中 {data.running_sessions ?? '?'}/{data.max_concurrent ?? '?'}）
		</div>{/if}
{:else if data.reply}
	<div class="tool-result-label">收到回复</div>
	<pre class="tool-result-preview">{typeof data.reply === 'string'
			? data.reply
			: JSON.stringify(data.reply, null, 2)}</pre>
{:else if typeof data.text === 'string' && data.text}
	<pre class="tool-result-preview">{data.text}</pre>
{:else}
	<pre class="tool-result-preview">{JSON.stringify(data, null, 2)}</pre>
{/if}

<style>
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
