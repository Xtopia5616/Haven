<script>
	/** @typedef {{ name: string; enabled: boolean; [key: string]: any }} ToggleItem */

	/** @type {ToggleItem[]} */
	let mcpServers = $state([]);
	/** @type {ToggleItem[]} */
	let skills = $state([]);
	/** @type {ToggleItem[]} */
	let builtinTools = $state([]);
	let activeTab = $state('builtin');
	let mcpDialogOpen = $state(false);
	let mcpEditServer = /** @type {Record<string, any> | null} */ ($state(null));

	import { onMount, onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import logger from '$lib/logger.ts';
	import { registerOne } from '$lib/events.ts';
	import SkillCard from '$lib/SkillCard.svelte';
	import McpServerCard from '$lib/McpServerCard.svelte';
	import McpEditDialog from '$lib/McpEditDialog.svelte';
	import BuiltinToolCard from '$lib/BuiltinToolCard.svelte';

	/** @type {{ dispose: () => void }} */
	let unlistenSkills;
	/** @type {{ dispose: () => void }} */
	let unlistenMcp;

	onMount(async () => {
		try {
			const result = await invoke('get_tools');
			if (result && result.tools) {
				const tools = /** @type {Array<any>} */ (result.tools);
				builtinTools = tools
					.map((t) => ({
						name: t.name || 'unknown',
						desc: t.description || '',
						risk: t.risk_level || 'unknown',
						schema: t.input_schema || {},
						enabled: t.enabled !== false,
					}))
					.sort((a, b) => a.name.localeCompare(b.name));
			}
		} catch (e) {
			builtinTools = [];
			addNotification(`加载工具列表失败: ${e}`, 'error', 3000);
		}
		await refreshMcpServers();
		await refreshSkillList();
		unlistenSkills = await registerOne('skills:status_change', async () => {
			await refreshSkillList();
		}, { tag: 'tools' });
		unlistenMcp = await registerOne('mcp:status_change', async () => {
			await refreshMcpServers();
		}, { tag: 'tools' });
	});

	onDestroy(() => {
		unlistenSkills?.dispose();
		unlistenMcp?.dispose();
	});

	async function refreshMcpServers() {
		try {
			const result = await invoke('list_mcp_tools');
			mcpServers = result || [];
			return true;
		} catch (e) {
			mcpServers = [];
			logger.warn('tools', 'list_mcp_tools error', e);
			return false;
		}
	}

	async function refreshMcpList() {
		// Diff-only refresh: check the persisted config against the live
		// clients and reconcile additions/removals/changed-config reconnects.
		// Already-connected servers with an unchanged config keep their live
		// session (no restart — e.g. Ghidra is not relaunched). Reconnecting a
		// specific server is the per-card Refresh button's job.
		try {
			const result = await invoke('refresh_mcp_servers');
			await refreshMcpServers();
			const added = result?.added || [];
			const removed = result?.removed || [];
			const updated = result?.updated || [];
			const failed = result?.failed || [];
			if (added.length === 0 && removed.length === 0 && updated.length === 0 && failed.length === 0) {
				addNotification('MCP 服务器无变化', 'info', 2000);
			} else {
				const parts = [];
				if (added.length) parts.push(`新增 ${added.join(', ')}`);
				if (removed.length) parts.push(`移除 ${removed.join(', ')}`);
				if (updated.length) parts.push(`重连 ${updated.join(', ')}`);
				if (failed.length) parts.push(`${failed.join(', ')} 连接失败`);
				addNotification(
					`MCP 刷新: ${parts.join('；')}`,
					failed.length && added.length === 0 && removed.length === 0 && updated.length === 0 ? 'warning' : 'success',
					3000,
				);
			}
		} catch (e) {
			addNotification(`刷新 MCP 服务器失败: ${e}`, 'error', 3000);
		}
	}

	async function refreshSkillList() {
		try {
			const result = await invoke('list_skills');
			skills = result || [];
		} catch (e) {
			skills = [];
			logger.warn('tools', 'list_skills error', e);
		}
	}

	// Shared optimistic toggle for skills / MCP servers / builtin tools:
	// flip the item locally, invoke the backend command, and roll back on
	// failure. `refresh` runs after a successful toggle. One implementation
	// so the three handlers cannot drift (e.g. one forgetting the refresh).
	/**
	 * @param {ToggleItem[]} list
	 * @param {string} name
	 * @param {boolean} enabled
	 * @param {(v: ToggleItem[]) => void} setList
	 * @param {string} invokeCmd
	 * @param {(() => void | Promise<any>) | null} refresh
	 */
	async function toggleItem(list, name, enabled, setList, invokeCmd, refresh) {
		const prev = list.map((x) => ({ ...x }));
		setList(list.map((x) => (x.name === name ? { ...x, enabled } : x)));
		try {
			await invoke(invokeCmd, { name, enabled });
			addNotification(
				`${name} 已${enabled ? '启用' : '禁用'}`,
				'success',
				2000,
			);
			if (refresh) await refresh();
		} catch (e) {
			setList(prev);
			addNotification(`切换 ${name} 失败: ${e}`, 'error', 3000);
		}
	}

	/**
	 * @param {string} name
	 * @param {boolean} enabled
	 */
	async function handleToggle(name, enabled) {
		await toggleItem(skills, name, enabled, (v) => (skills = v), 'set_skill_enabled', null);
	}

	async function refreshSkills() {
		try {
			await invoke('refresh_skills');
			await refreshSkillList();
			addNotification('技能已刷新', 'success', 2000);
		} catch (e) {
			addNotification(`刷新技能失败: ${e}`, 'error', 3000);
		}
	}

	async function openFolder() {
		try {
			const path = await invoke('open_skills_dir');
			addNotification(`已打开: ${path}`, 'info', 3000);
		} catch (e) {
			addNotification(`打开技能文件夹失败: ${e}`, 'error', 3000);
		}
	}

	function openAddDialog() {
		mcpEditServer = null;
		mcpDialogOpen = true;
	}

	/**
	 * @param {Record<string, any>} server
	 */
	function openEditDialog(server) {
		mcpEditServer = server;
		mcpDialogOpen = true;
	}

	function closeDialog() {
		mcpDialogOpen = false;
		mcpEditServer = null;
	}

	/**
	 * @param {Record<string, any>} config
	 */
	async function handleSave(config) {
		try {
			if (mcpEditServer) {
				await invoke('update_mcp_server', { name: mcpEditServer.name, config });
				addNotification(`已更新 ${config.name}`, 'success', 2000);
			} else {
				await invoke('add_mcp_server', { config });
				addNotification(`已添加 ${config.name}`, 'success', 2000);
			}
			closeDialog();
			await refreshMcpServers();
		} catch (e) {
			addNotification(`操作失败: ${e}`, 'error', 4000);
		}
	}

	/**
	 * @param {string} name
	 */
	async function handleRemove(name) {
		try {
			await invoke('remove_mcp_server', { name });
			addNotification(`已移除 ${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			addNotification(`移除失败: ${e}`, 'error', 3000);
		}
	}

	/**
	 * @param {string} name
	 */
	async function handleReconnect(name) {
		addNotification(`正在刷新 ${name}…`, 'info', 1500);
		try {
			await invoke('reconnect_mcp', { name });
			addNotification(`刷新成功：${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			addNotification(`刷新失败: ${e}`, 'error', 3000);
		}
	}

	/**
	 * @param {string} name
	 * @param {boolean} enabled
	 */
	async function handleMcpToggle(name, enabled) {
		await toggleItem(
			mcpServers,
			name,
			enabled,
			(v) => (mcpServers = v),
			'toggle_mcp_server',
			refreshMcpServers,
		);
	}

	/**
	 * @param {string} name
	 * @param {boolean} enabled
	 */
	async function handleToolToggle(name, enabled) {
		await toggleItem(
			builtinTools,
			name,
			enabled,
			(v) => (builtinTools = v),
			'set_tool_enabled',
			null,
		);
	}

	const tabs = [
		{ id: 'builtin', label: 'BUILTIN' },
		{ id: 'mcp', label: 'MCP' },
		{ id: 'skills', label: 'SKILLS' },
	];
</script>

<div class="tools-page">
	<h1>Tools</h1>

	<div class="md-tabs" role="tablist">
		{#each tabs as tab}
			<button
				class="md-tab"
				class:active={activeTab === tab.id}
				role="tab"
				aria-selected={activeTab === tab.id}
				onclick={() => (activeTab = tab.id)}
			>
				{tab.label}
			</button>
		{/each}
	</div>

	{#if activeTab === 'builtin'}
		<div class="section">
			<div class="toolbar">
				<h2>Built-in Tools</h2>
			</div>
			{#if builtinTools.length === 0}
				<div class="empty-state">
					<p>No built-in tools available</p>
				</div>
			{:else}
				<div class="server-list">
					{#each builtinTools as tool (tool.name)}
						<BuiltinToolCard {tool} onToggle={handleToolToggle} />
					{/each}
				</div>
			{/if}
		</div>
	{:else if activeTab === 'mcp'}
		<div class="section">
			<div class="toolbar">
				<h2>MCP Servers</h2>
				<div class="toolbar-actions">
					<button class="md-btn md-btn--outlined" onclick={refreshMcpList}>Refresh</button>
					<button class="md-btn md-btn--outlined" onclick={openAddDialog}>
						<svg viewBox="0 0 24 24" fill="currentColor"><path d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6z"/></svg>
						Add
					</button>
				</div>
			</div>
			{#if mcpServers.length === 0}
				<div class="empty-state">
					<p>No MCP servers configured</p>
					<p class="hint">
						Add an MCP server to extend the agent with external tools and resources.
					</p>
					<button class="md-btn md-btn--filled" onclick={openAddDialog}>Add MCP Server</button>
				</div>
			{:else}
				<div class="server-list">
					{#each mcpServers as server (server.name)}
						<McpServerCard
							{server}
							onEdit={openEditDialog}
							onRemove={handleRemove}
							onReconnect={handleReconnect}
							onToggle={handleMcpToggle}
						/>
					{/each}
				</div>
			{/if}
		</div>
	{:else}
		<div class="section">
			<div class="toolbar">
				<h2>Skills</h2>
				<div class="toolbar-actions">
					<button class="md-btn md-btn--outlined" onclick={refreshSkills}>Refresh</button>
					<button class="md-btn md-btn--outlined" onclick={openFolder}>Open Folder</button>
				</div>
			</div>
			{#if skills.length === 0}
				<div class="empty-state">
					<p>No skills found</p>
					<p class="hint">
						Place SKILL.md files in your skills folder, then click Refresh.
					</p>
					<button class="md-btn md-btn--filled" onclick={openFolder}>Open Skills Folder</button>
				</div>
			{:else}
				<div class="server-list">
					{#each skills as skill (skill.name)}
						<SkillCard {skill} onToggle={handleToggle} />
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</div>

{#if mcpDialogOpen}
	<McpEditDialog
		server={mcpEditServer}
		onClose={closeDialog}
		onSave={handleSave}
		existingNames={mcpServers.map((s) => s.name)}
	/>
{/if}

<style>
	.tools-page {
		max-width: var(--md-sys-content-max-width);
	}
	h1 {
		font-size: 24px;
		font-weight: 600;
		margin-bottom: var(--md-sys-space-xl);
		color: var(--md-sys-color-on-surface);
	}
	.md-tabs {
		margin-bottom: var(--md-sys-space-xl);
	}
	.section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-lg);
	}
	.section:last-child {
		margin-bottom: 0;
	}
	.section h2 {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 1px;
		margin-bottom: var(--md-sys-space-lg);
	}
	.empty-state {
		text-align: center;
		padding: 48px 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
	}
	.empty-state p {
		margin: 0;
	}
	.server-list {
		display: flex;
		flex-direction: column;
	}
	.toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: var(--md-sys-space-lg);
		min-height: var(--md-comp-button-small-height);
	}
	.toolbar h2 {
		margin: 0;
		line-height: var(--md-comp-button-small-height);
	}
	.toolbar-actions {
		display: flex;
		gap: var(--md-sys-space-sm);
	}
	.hint {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
		max-width: 320px;
	}
</style>
