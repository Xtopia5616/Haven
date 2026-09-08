<script>
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();

	/** @param {unknown} value */
	function statusLabel(value) {
		return value ? '已连接' : '未连接';
	}
</script>

{#if Array.isArray(data.servers)}
	<div class="tool-card-count">{data.servers.length} 个 MCP 服务</div>
	<div class="admin-list">
		{#each data.servers as server, index (server.name ?? index)}
			<div class="admin-row">
				<span class="admin-name">{server.name || '未命名服务'}</span>
				<span class:admin-ok={server.connected} class="admin-state">{statusLabel(server.connected)}</span>
				{#if server.tools != null}<span class="admin-meta">{server.tools} 个工具</span>{/if}
			</div>
		{/each}
	</div>
{:else if Array.isArray(data.skills)}
	<div class="tool-card-count">{data.skills.length} 个技能</div>
	<div class="admin-list">
		{#each data.skills as skill, index (skill.name ?? index)}
			<div class="admin-row">
				<span class="admin-name">{skill.name || '未命名技能'}</span>
				<span class:admin-ok={skill.enabled} class="admin-state">{skill.enabled ? '已启用' : '已停用'}</span>
			</div>
		{/each}
	</div>
{:else if Array.isArray(data.sessions) || Array.isArray(data.errors)}
	{@const rows = Array.isArray(data.sessions) ? data.sessions : data.errors}
	<div class="tool-card-count">{rows.length} 条记录</div>
	{#if rows.length === 0}<p class="tool-card-empty">没有记录</p>{/if}
	<div class="admin-list">
		{#each rows as row, index (row.id ?? index)}
			<div class="admin-row">
				<span class="admin-name">{row.title || row.id || '未命名会话'}</span>
				{#if row.status}<span class="admin-state">{row.status}</span>{/if}
			</div>
		{/each}
	</div>
{:else if data.level && data.saved}
	<div class="admin-action"><span class="admin-badge">已保存</span><span>日志级别：{data.level}</span></div>
{:else if data.name && data.enabled != null}
	<div class="admin-action"><span class="admin-badge">{data.enabled ? '已启用' : '已停用'}</span><span>{data.name}</span></div>
{:else if data.name && (data.connected != null || data.created || data.removed)}
	<div class="admin-action">
		<span class="admin-badge">{data.removed ? '已移除' : data.created ? '已创建' : statusLabel(data.connected)}</span>
		<span>{data.name}</span>
	</div>
{:else}
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.tool-card-count,
	.tool-card-empty { font-size: var(--md-sys-typescale-label-small-size); line-height: var(--md-sys-typescale-label-small-line-height); color: var(--md-sys-color-on-surface-variant); }
	.tool-card-count { font-weight: 600; margin-bottom: var(--md-sys-space-xs); }
	.tool-card-empty { margin: 0; }
	.admin-list { max-height: 220px; overflow-y: auto; border-radius: var(--md-sys-shape-extra-small); }
	.admin-row { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); padding: 4px var(--md-sys-space-2xs); border-radius: 4px; }
	.admin-row:nth-child(odd) { background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent); }
	.admin-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--md-sys-color-on-surface); font-family: var(--md-sys-typescale-mono); font-size: var(--md-sys-typescale-code-size); }
	.admin-state { flex: none; color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); }
	.admin-ok { color: var(--md-sys-color-success); }
	.admin-meta { flex: none; color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); }
	.admin-action { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); color: var(--md-sys-color-on-surface); }
	.admin-badge { flex: none; padding: 1px 6px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: var(--md-sys-typescale-label-small-size); font-weight: 700; }
</style>
