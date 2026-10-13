<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type { SessionStatus } from './contracts/generatedCommands.ts';

	interface AdminSkillRow extends Record<string, unknown> {
		name?: string | null;
		enabled?: boolean | null;
		executable?: boolean | null;
		unavailable_reason?: string | null;
		manifest_error?: string | null;
	}

	interface AdminActivityRow extends Record<string, unknown> {
		session_id: string;
		title?: string | null;
		status?: SessionStatus;
	}

	interface Props {
		data?: {
			skills?: AdminSkillRow[];
			sessions?: AdminActivityRow[];
			errors?: AdminActivityRow[];
			level?: string | null;
			saved?: boolean | null;
			name?: string | null;
			enabled?: boolean | null;
			connected?: boolean | unknown[] | null;
			created?: boolean | null;
			removed?: boolean | null;
			path?: string | null;
			error?: string | null;
			total_lines?: number | null;
			lines?: string[];
		};
	}

	let { data = {} }: Props = $props();

	function statusLabel(value: unknown) {
		return value ? '已连接' : '未连接';
	}
</script>

{#if Array.isArray(data.skills)}
	<div class="tool-result-label">{data.skills.length} 个技能</div>
	<ToolResultList items={data.skills}>
		{#snippet children(visibleSkills)}
			<div class="admin-list tool-result-scroll-area">
				{#each visibleSkills as skill (skill.name)}
					<div class="admin-row">
						<span class="admin-name">{skill.name || '未命名技能'}</span>
						<span class:admin-ok={skill.executable} class="admin-state"
							>{skill.manifest_error
								? '清单无效'
								: skill.executable
									? '已启用'
									: skill.enabled
										? '配置已启用，当前不可执行'
										: '已停用'}</span
						>
					</div>
					{#if skill.manifest_error || skill.unavailable_reason}
						<p class="tool-result-message tool-result-message--compact">
							{skill.manifest_error ?? skill.unavailable_reason}
						</p>
					{/if}
				{/each}
			</div>
		{/snippet}
	</ToolResultList>
{:else if Array.isArray(data.lines)}
	<div class="tool-result-label">日志 · {data.total_lines ?? data.lines.length} 行</div>
	{#if data.path}<div class="tool-result-meta">{data.path}</div>{/if}
	{#if data.lines.length > 0}
		<pre class="tool-result-preview">{data.lines.join('\n')}</pre>
	{:else}
		<p class="tool-result-message tool-result-message--compact">
			日志文件存在，但没有可显示的日志行。
		</p>
	{/if}
{:else if data.path && data.error}
	<div class="tool-result-label">日志不可用</div>
	<p class="tool-result-message">{data.error}</p>
	<div class="tool-result-meta">{data.path}</div>
{:else if Array.isArray(data.sessions) || Array.isArray(data.errors)}
	{@const rows = Array.isArray(data.sessions) ? data.sessions : (data.errors ?? [])}
	<div class="tool-result-label">{rows.length} 条记录</div>
	{#if rows.length === 0}<p class="tool-result-message tool-result-message--compact">
			没有记录
		</p>{/if}
	<ToolResultList items={rows}>
		{#snippet children(visibleRows: Array<Record<string, unknown>>)}
			<div class="admin-list tool-result-scroll-area">
				{#each visibleRows as row (row.session_id)}
					<div class="admin-row">
						<span class="admin-name">{row.title || row.session_id || '未命名会话'}</span
						>
						{#if row.status}<span class="admin-state">{row.status}</span>{/if}
					</div>
				{/each}
			</div>
		{/snippet}
	</ToolResultList>
{:else if data.level && data.saved}
	<div class="admin-action">
		<span class="admin-badge">已保存</span><span>日志级别：{data.level}</span>
	</div>
{:else if data.name && data.enabled != null}
	<div class="admin-action">
		<span class="admin-badge">{data.enabled ? '已启用' : '已停用'}</span><span>{data.name}</span
		>
	</div>
{:else if data.name && (data.connected != null || data.created || data.removed)}
	<div class="admin-action">
		<span class="admin-badge"
			>{data.removed ? '已移除' : data.created ? '已创建' : statusLabel(data.connected)}</span
		>
		<span>{data.name}</span>
	</div>
{:else}
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.admin-list {
		--tool-result-scroll-max-height: 220px;
	}
	.tool-result-preview {
		max-height: 320px;
		overflow: auto;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.admin-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 4px var(--md-sys-space-2xs);
		border-radius: 4px;
	}
	.admin-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.admin-name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
	}
	.admin-state {
		flex: none;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
	}
	.admin-ok {
		color: var(--md-sys-color-success);
	}
	.admin-action {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface);
	}
	.admin-badge {
		flex: none;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
	}
</style>
