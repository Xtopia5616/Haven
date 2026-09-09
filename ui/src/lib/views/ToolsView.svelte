<script>
	/** @typedef {{ name: string; enabled: boolean; [key: string]: any }} ToggleItem */

	/** @type {ToggleItem[]} */
	let mcpServers = $state([]);
	/** @type {ToggleItem[]} */
	let skills = $state([]);
	/** @type {ToggleItem[]} */
	let builtinTools = $state([]);
	let activeTab = $state('builtin');
	let searchQuery = $state('');
	let enabledFilter = $state('all');
	let mcpDialogOpen = $state(false);
	let mcpEditServer = /** @type {Record<string, any> | null} */ ($state(null));

	import { onMount, onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import logger from '$lib/logger.ts';
	import { registerOne } from '$lib/events.ts';
	import SkillCard from '$lib/SkillCard.svelte';
	import McpServerCard from '$lib/McpServerCard.svelte';
	import McpEditDialog from '$lib/McpEditDialog.svelte';
	import BuiltinToolCard from '$lib/BuiltinToolCard.svelte';
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';

	/** @type {{ dispose: () => void }} */
	let unlistenSkills;
	/** @type {{ dispose: () => void }} */
	let unlistenMcp;
	/** @type {ReturnType<typeof setTimeout> | null} */
	let mcpRefreshTimer = null;
	let mcpRefreshing = $state(false);
	let skillsRefreshing = $state(false);

	/** @param {Record<string, any>} item */
	function matchesResource(item) {
		const query = searchQuery.trim().toLocaleLowerCase();
		if (enabledFilter === 'enabled' && item.enabled === false) return false;
		if (enabledFilter === 'disabled' && item.enabled !== false) return false;
		if (!query) return true;
		const text = [item.name, item.desc, item.description, item.url, item.transport]
			.filter(Boolean)
			.join(' ')
			.toLocaleLowerCase();
		return text.includes(query);
	}

	/** @param {string} value */
	function handleEnabledFilterChange(value) {
		enabledFilter = value;
	}

	function clearFilters() {
		searchQuery = '';
		enabledFilter = 'all';
	}

	const visibleBuiltinTools = $derived(builtinTools.filter(matchesResource));
	const visibleMcpServers = $derived(mcpServers.filter(matchesResource));
	const visibleSkills = $derived(skills.filter(matchesResource));
	const hasFilters = $derived(Boolean(searchQuery.trim() || enabledFilter !== 'all'));
	const activeResourceCount = $derived(
		activeTab === 'builtin'
			? visibleBuiltinTools.length
			: activeTab === 'mcp'
				? visibleMcpServers.length
				: visibleSkills.length,
	);

	function scheduleMcpRefresh() {
		// Cold start emits Connecting+Connected per server; coalesce into one
		// list_mcp_tools round-trip instead of 2N full snapshots.
		if (mcpRefreshTimer) clearTimeout(mcpRefreshTimer);
		mcpRefreshTimer = setTimeout(() => {
			mcpRefreshTimer = null;
			refreshMcpServers();
		}, 120);
	}

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
			reportError(e, { context: 'ToolsView', message: '加载工具列表失败', log: false });
		}
		await refreshMcpServers();
		await refreshSkillList();
		unlistenSkills = await registerOne(
			'skills:status_change',
			async () => {
				await refreshSkillList();
			},
			{ tag: 'tools' },
		);
		unlistenMcp = await registerOne(
			'mcp:status_change',
			() => {
				scheduleMcpRefresh();
			},
			{ tag: 'tools' },
		);
	});

	onDestroy(() => {
		if (mcpRefreshTimer) clearTimeout(mcpRefreshTimer);
		unlistenSkills?.dispose();
		unlistenMcp?.dispose();
	});

	async function refreshMcpServers() {
		try {
			const result = await invoke('list_mcp_tools');
			mcpServers = result || [];
			return true;
		} catch (e) {
			logger.warn('tools', 'list_mcp_tools error', e);
			return false;
		}
	}

	async function resetToolCircuits() {
		try {
			await invoke('reset_tool_circuits');
			addNotification('工具熔断已重置', 'success', 2500);
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '重置工具熔断失败', log: false });
		}
	}

	async function refreshMcpList() {
		if (mcpRefreshing) return;
		mcpRefreshing = true;
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
			if (
				added.length === 0 &&
				removed.length === 0 &&
				updated.length === 0 &&
				failed.length === 0
			) {
				addNotification('MCP 服务器无变化', 'info', 2000);
			} else {
				const parts = [];
				if (added.length) parts.push(`新增 ${added.join(', ')}`);
				if (removed.length) parts.push(`移除 ${removed.join(', ')}`);
				if (updated.length) parts.push(`重连 ${updated.join(', ')}`);
				if (failed.length) parts.push(`${failed.join(', ')} 连接失败`);
				addNotification(
					`MCP 刷新: ${parts.join('；')}`,
					failed.length &&
						added.length === 0 &&
						removed.length === 0 &&
						updated.length === 0
						? 'warning'
						: 'success',
					3000,
				);
			}
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '刷新 MCP 服务器失败', log: false });
		} finally {
			mcpRefreshing = false;
		}
	}

	async function refreshSkillList() {
		try {
			const result = await invoke('list_skills');
			skills = result || [];
		} catch (e) {
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
			addNotification(`${name} 已${enabled ? '启用' : '禁用'}`, 'success', 2000);
			if (refresh) await refresh();
		} catch (e) {
			setList(prev);
			reportError(e, { context: 'ToolsView', message: `切换 ${name} 失败`, log: false });
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
		if (skillsRefreshing) return;
		skillsRefreshing = true;
		try {
			await invoke('refresh_skills');
			await refreshSkillList();
			addNotification('技能已刷新', 'success', 2000);
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '刷新技能失败', log: false });
		} finally {
			skillsRefreshing = false;
		}
	}

	async function openFolder() {
		try {
			const path = await invoke('open_skills_dir');
			addNotification(`已打开: ${path}`, 'info', 3000);
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '打开技能文件夹失败', log: false });
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
			reportError(e, { context: 'ToolsView', message: '操作失败', log: false });
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
			reportError(e, { context: 'ToolsView', message: '移除失败', log: false });
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
			reportError(e, { context: 'ToolsView', message: '刷新失败', log: false });
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
		{ id: 'builtin', label: '内置工具' },
		{ id: 'mcp', label: 'MCP' },
		{ id: 'skills', label: '技能' },
	];
	/** @param {string} tabId */
	function selectToolTab(tabId) {
		activeTab = tabId;
	}
</script>

<div class="tools-page">
	<WorkspacePageHeader title="工具" description="管理 Haven 可调用的工具、MCP 服务与技能。" />

	<MaterialTabs
		{tabs}
		{activeTab}
		onNavigate={selectToolTab}
		ariaLabel="工具分类"
		idPrefix="tools-tab"
		panelIdPrefix=""
		className="workspace-secondary-tabs"
	/>

	<div class="resource-toolbar workspace-filter-bar" role="search" aria-label="筛选工具资源">
		<label class="resource-search">
			<span class="sr-only">搜索工具资源</span>
			<input
				class="md-input"
				type="search"
				bind:value={searchQuery}
				placeholder="搜索名称、描述或地址"
			/>
		</label>
		<div class="resource-filter-controls">
			<MaterialSelect
				id="resource-enabled-filter"
				value={enabledFilter}
				ariaLabel="启用状态"
				options={[
					{ value: 'all', label: '全部状态' },
					{ value: 'enabled', label: '仅启用' },
					{ value: 'disabled', label: '仅禁用' },
				]}
				onChange={handleEnabledFilterChange}
			/>
			{#if hasFilters}
				<MaterialButton variant="text" label="清除" onclick={clearFilters} />
			{/if}
		</div>
		<CountChip count={activeResourceCount} label="项" className="resource-count" live />
	</div>

	{#if activeTab === 'builtin'}
		<section class="resource-panel motion-surface-enter" aria-label="内置工具">
			<div class="resource-heading">
				<div class="resource-heading-copy">
					<h2>内置工具</h2>
					<p>Haven 自带的可调用能力，可以单独启用或停用。</p>
				</div>
				<div class="toolbar-actions toolbar-actions--paired">
					<MaterialButton
						variant="outlined"
						label="重置熔断"
						onclick={resetToolCircuits}
					/>
				</div>
			</div>
			{#if builtinTools.length === 0}
				<AsyncState
					title="暂无可用的内置工具"
					message="工具列表加载后，可在此查看详情与启用状态。"
				/>
			{:else if visibleBuiltinTools.length === 0}
				<AsyncState title="没有匹配的内置工具" message="换一个关键词或清除状态筛选。" />
			{:else}
				<div class="resource-list">
					{#each visibleBuiltinTools as tool (tool.name)}
						<BuiltinToolCard {tool} onToggle={handleToolToggle} />
					{/each}
				</div>
			{/if}
		</section>
	{:else if activeTab === 'mcp'}
		<section class="resource-panel motion-surface-enter" aria-label="MCP 服务器">
			<div class="resource-heading">
				<div class="resource-heading-copy">
					<h2>MCP 服务器</h2>
					<p>连接外部工具服务，并查看当前连接状态。</p>
				</div>
				<div class="toolbar-actions toolbar-actions--paired">
					<RefreshButton loading={mcpRefreshing} onclick={refreshMcpList} />
					<MaterialButton variant="outlined" label="添加" onclick={openAddDialog} />
				</div>
			</div>
			{#if mcpServers.length === 0}
				<AsyncState
					title="尚未配置 MCP 服务器"
					message="添加 MCP 服务器，为 Agent 扩展外部工具与资源。"
					actionLabel="添加 MCP 服务器"
					onAction={openAddDialog}
				/>
			{:else if visibleMcpServers.length === 0}
				<AsyncState title="没有匹配的 MCP 服务器" message="换一个关键词或清除状态筛选。" />
			{:else}
				<div class="resource-list">
					{#each visibleMcpServers as server (server.name)}
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
		</section>
	{:else}
		<section class="resource-panel motion-surface-enter" aria-label="技能">
			<div class="resource-heading">
				<div class="resource-heading-copy">
					<h2>技能</h2>
					<p>管理可被 Agent 调用的技能和执行脚本。</p>
				</div>
				<div class="toolbar-actions toolbar-actions--paired">
					<RefreshButton loading={skillsRefreshing} onclick={refreshSkills} />
					<MaterialButton variant="outlined" label="打开文件夹" onclick={openFolder} />
				</div>
			</div>
			{#if skills.length === 0}
				<AsyncState
					title="暂无技能"
					message="将 SKILL.md 文件放入技能文件夹，然后点击刷新。"
					actionLabel="打开技能文件夹"
					onAction={openFolder}
				/>
			{:else if visibleSkills.length === 0}
				<AsyncState title="没有匹配的技能" message="换一个关键词或清除状态筛选。" />
			{:else}
				<div class="resource-list">
					{#each visibleSkills as skill (skill.name)}
						<SkillCard {skill} onToggle={handleToggle} />
					{/each}
				</div>
			{/if}
		</section>
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
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
	}
	.resource-toolbar {
		margin-bottom: var(--md-sys-space-xl);
	}
	.resource-search {
		flex: 1 1 280px;
		min-width: 0;
	}
	.resource-filter-controls {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.resource-filter-controls :global(.md-select-container) {
		width: 140px;
		flex-shrink: 0;
	}
	:global(.resource-count) {
		flex: 0 0 auto;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.resource-panel {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-lg);
		min-width: 0;
	}
	.resource-heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.resource-heading-copy {
		flex: 1 1 0;
		min-width: 0;
	}
	.resource-heading h2 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.resource-heading p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.resource-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-actions--paired {
		flex: 0 0 auto;
	}
	.toolbar-actions--paired :global(.md-btn) {
		flex: 1 1 0;
		min-width: 0;
		white-space: nowrap;
	}
	.toolbar-actions--paired :global(.refresh-button) {
		flex: 0 0 var(--md-comp-refresh-button-width);
	}
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
	@media (max-width: 700px) {
		.resource-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.resource-search,
		.resource-filter-controls {
			width: 100%;
		}
		.resource-search {
			flex: 0 1 auto;
		}
		.resource-filter-controls {
			align-items: stretch;
			flex-direction: column;
			gap: var(--md-sys-space-sm);
		}
		.resource-filter-controls :global(.md-select-container) {
			width: 100%;
		}
		.toolbar-actions {
			width: 100%;
		}
		.toolbar-actions--paired {
			flex: 0 0 100%;
		}
		.toolbar-actions--paired :global(.refresh-button) {
			flex: 1 1 0;
		}
		.resource-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		.toolbar-actions :global(.md-btn) {
			flex: 1 1 0;
		}
	}
	@media (max-width: 455px) {
		:global(.resource-count) {
			align-self: flex-start;
		}
		.toolbar-actions {
			flex-direction: column;
		}
		.toolbar-actions :global(.md-btn) {
			width: 100%;
			flex: 0 0 var(--md-comp-button-small-height);
		}
		.resource-heading {
			align-items: flex-start;
			flex-direction: column;
		}
		.toolbar-actions--paired {
			width: 100%;
		}
	}
</style>
