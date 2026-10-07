<script lang="ts">
	let { isVisible = true }: { isVisible?: boolean } = $props();
	let mcpServers = $state<McpServerSnapshot[]>([]);
	let skills = $state<SkillInfo[]>([]);
	let builtinTools = $state<BuiltinToolEntry[]>([]);
	let activeTab = $state<'builtin' | 'mcp' | 'skills'>('builtin');
	let searchQuery = $state('');
	let enabledFilter = $state<'all' | 'enabled' | 'disabled'>('all');
	let mcpDialogOpen = $state(false);
	let mcpEditServer = $state<McpServerSnapshot | null>(null);

	import { onMount, onDestroy } from 'svelte';
	import {
		addMcpServer,
		openSkillsDir,
		reconnectMcp,
		refreshMcpServers as refreshMcpServersCommand,
		refreshSkills as refreshSkillsCommand,
		removeMcpServer,
		setSkillEnabled,
		setToolEnabled,
		toggleMcpServer,
		updateMcpServer,
		listBuiltinToolManifests,
		listMcpTools,
		listSkills,
		resetToolCircuits as resetToolCircuitsCommand,
	} from '$lib/toolsCommands.ts';
	import { addNotification } from '$lib/notificationStore.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { isAuthorizationConfirmationPending } from '$lib/formatError.ts';
	import { registerAppListener } from '$lib/events.ts';
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
	import WorkspaceSectionHeader from '$lib/WorkspaceSectionHeader.svelte';
	import {
		builtinToolEntryFromManifest,
		filterBuiltinToolCard,
		groupBuiltinTools,
	} from '$lib/builtinToolPresentation.ts';
	import { setToolManifests } from '$lib/toolManifest.ts';
	import type { BuiltinToolEntry } from '$lib/builtinToolPresentation.ts';
	import type {
		McpServerConfigInput,
		McpServerSnapshot,
	} from '$lib/contracts/generatedCommands.ts';
	import type { SkillInfo } from '$lib/contracts/tools.ts';

	interface ResourceFilterItem {
		name: string;
		enabled: boolean;
		desc?: string;
		description?: string;
		url?: string;
		transport?: string;
	}

	let unlistenSkills: { dispose: () => void } | undefined;
	let unlistenMcp: { dispose: () => void } | undefined;
	let mcpRefreshTimer: ReturnType<typeof setTimeout> | null = null;
	let mcpRefreshing = $state(false);
	let skillsRefreshing = $state(false);

	function matchesResource(item: ResourceFilterItem) {
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

	function handleEnabledFilterChange(value: string) {
		if (value === 'all' || value === 'enabled' || value === 'disabled') enabledFilter = value;
	}

	function clearFilters() {
		searchQuery = '';
		enabledFilter = 'all';
	}

	const builtinToolCards = $derived(groupBuiltinTools(builtinTools));
	const visibleBuiltinTools = $derived(
		builtinToolCards
			.map((card) => filterBuiltinToolCard(card, searchQuery, enabledFilter))
			.filter((card) => card !== null),
	);
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
			refreshMcpServers(false);
		}, 120);
	}

	onMount(async () => {
		try {
			const result = await listBuiltinToolManifests();
			if (result && result.tools) {
				const manifests = setToolManifests(result.tools);
				builtinTools = manifests
					.map(builtinToolEntryFromManifest)
					.sort((a, b) => a.name.localeCompare(b.name));
			}
		} catch (e) {
			builtinTools = [];
			reportError(e, { context: 'ToolsView', message: '加载工具列表失败', log: false });
		}
		await refreshMcpServers();
		await refreshSkillList();
		unlistenSkills = await registerAppListener(
			'skills:status_change',
			async () => {
				await refreshSkillList(false);
			},
			{ tag: 'tools' },
		);
		unlistenMcp = await registerAppListener(
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

	async function refreshMcpServers(notifyOnError = true) {
		try {
			const result = await listMcpTools();
			mcpServers = result || [];
			return true;
		} catch (error) {
			reportError(error, {
				context: 'ToolsView',
				message: '加载 MCP 服务器失败',
				log: false,
				notify: notifyOnError,
			});
			return false;
		}
	}

	async function resetToolCircuits() {
		try {
			await resetToolCircuitsCommand();
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
			const result = await refreshMcpServersCommand();
			if (!(await refreshMcpServers())) return;
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
			if (isAuthorizationConfirmationPending(e)) return;
			reportError(e, { context: 'ToolsView', message: '刷新 MCP 服务器失败', log: false });
		} finally {
			mcpRefreshing = false;
		}
	}

	async function refreshSkillList(notifyOnError = true) {
		try {
			const result = await listSkills();
			skills = result || [];
			return true;
		} catch (error) {
			reportError(error, {
				context: 'ToolsView',
				message: '加载技能列表失败',
				log: false,
				notify: notifyOnError,
			});
			return false;
		}
	}

	// Shared optimistic toggle for skills / MCP servers / builtin tools:
	// flip the item locally, invoke the backend command, and roll back on
	// failure. `refresh` runs after a successful toggle. One implementation
	// so the three handlers cannot drift (e.g. one forgetting the refresh).
	async function toggleItem<T extends { name: string; enabled: boolean }>(
		list: T[],
		name: string,
		enabled: boolean,
		setList: (items: T[]) => void,
		update: () => Promise<void>,
		refresh: (() => void | Promise<unknown>) | null,
	) {
		const prev = list.map((x) => ({ ...x }));
		setList(list.map((x) => (x.name === name ? { ...x, enabled } : x)));
		try {
			await update();
			addNotification(`${name} 已${enabled ? '启用' : '禁用'}`, 'success', 2000);
			if (refresh) await refresh();
		} catch (e) {
			setList(prev);
			reportError(e, { context: 'ToolsView', message: `切换 ${name} 失败`, log: false });
		}
	}

	async function handleToggle(name: string, enabled: boolean) {
		await toggleItem(
			skills,
			name,
			enabled,
			(v) => (skills = v),
			() => setSkillEnabled({ name, enabled }),
			null,
		);
	}

	async function refreshSkills() {
		if (skillsRefreshing) return;
		skillsRefreshing = true;
		try {
			await refreshSkillsCommand();
			if (!(await refreshSkillList())) return;
			addNotification('技能已刷新', 'success', 2000);
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '刷新技能失败', log: false });
		} finally {
			skillsRefreshing = false;
		}
	}

	async function openFolder() {
		try {
			const path = await openSkillsDir();
			addNotification(`已打开: ${path}`, 'info', 3000);
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '打开技能文件夹失败', log: false });
		}
	}

	function openAddDialog() {
		mcpEditServer = null;
		mcpDialogOpen = true;
	}

	function openEditDialog(server: McpServerSnapshot) {
		mcpEditServer = server;
		mcpDialogOpen = true;
	}

	function closeDialog() {
		mcpDialogOpen = false;
		mcpEditServer = null;
	}

	async function handleSave(config: McpServerConfigInput) {
		try {
			if (mcpEditServer) {
				await updateMcpServer({ name: mcpEditServer.name, config });
				addNotification(`已更新 ${config.name}`, 'success', 2000);
			} else {
				await addMcpServer(config);
				addNotification(`已添加 ${config.name}`, 'success', 2000);
			}
			closeDialog();
			await refreshMcpServers();
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '操作失败', log: false });
		}
	}

	async function handleRemove(name: string) {
		try {
			await removeMcpServer({ name });
			addNotification(`已移除 ${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			reportError(e, { context: 'ToolsView', message: '移除失败', log: false });
		}
	}

	async function handleReconnect(name: string) {
		try {
			await reconnectMcp({ name });
			addNotification(`刷新成功：${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			if (isAuthorizationConfirmationPending(e)) return;
			reportError(e, { context: 'ToolsView', message: '刷新失败', log: false });
		}
	}

	async function handleMcpToggle(name: string, enabled: boolean) {
		await toggleItem(
			mcpServers,
			name,
			enabled,
			(v) => (mcpServers = v),
			() => toggleMcpServer({ name, enabled }),
			refreshMcpServers,
		);
	}

	async function handleToolToggle(name: string, enabled: boolean) {
		await toggleItem(
			builtinTools,
			name,
			enabled,
			(v) => (builtinTools = v),
			() => setToolEnabled({ name, enabled }),
			null,
		);
	}

	const tabs = [
		{ id: 'builtin', label: '内置工具' },
		{ id: 'mcp', label: 'MCP' },
		{ id: 'skills', label: '技能' },
	];
	function selectToolTab(tabId: string) {
		if (tabId === 'builtin' || tabId === 'mcp' || tabId === 'skills') activeTab = tabId;
	}
</script>

<div class="tools-page">
	<WorkspacePageHeader title="工具" description="管理 Haven 可调用的工具、MCP 服务与技能。" />

	<div class="tools-workspace workspace-secondary-layout responsive-layout-transition">
		<nav
			class="tools-sidebar workspace-secondary-sidebar responsive-layout-panel"
			aria-label="资源分类导航"
		>
			<MaterialTabs
				{tabs}
				{activeTab}
				onNavigate={selectToolTab}
				ariaLabel="工具分类"
				idPrefix="tools-tab"
				panelIdPrefix=""
				className="workspace-secondary-tabs workspace-secondary-tabs--sidebar"
				{isVisible}
			/>
		</nav>

		<div class="tools-workspace-main workspace-secondary-main">
			{#snippet resourceToolbar()}
				<div
					class="resource-toolbar workspace-filter-bar"
					role="search"
					aria-label="筛选工具资源"
				>
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
							width="compact"
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
					<CountChip
						count={activeResourceCount}
						label="项"
						className="resource-count"
						live
					/>
				</div>
			{/snippet}

			{#if activeTab === 'builtin'}
				<section class="resource-panel motion-surface-enter" aria-label="内置工具">
					<WorkspaceSectionHeader
						title="内置工具"
						description="Haven 自带的可调用能力；按能力族、根能力和具体操作三级收纳，可展开后分别启停。"
					>
						{#snippet children()}
							<div class="toolbar-actions toolbar-actions--paired">
								<MaterialButton
									variant="outlined"
									label="重置熔断"
									onclick={resetToolCircuits}
								/>
							</div>
						{/snippet}
					</WorkspaceSectionHeader>
					{@render resourceToolbar()}
					{#if builtinTools.length === 0}
						<AsyncState
							title="暂无可用的内置工具"
							message="工具列表加载后，可在此查看详情与启用状态。"
						/>
					{:else if visibleBuiltinTools.length === 0}
						<AsyncState
							title="没有匹配的内置工具"
							message="换一个关键词或清除状态筛选。"
						/>
					{:else}
						<div class="resource-list resource-list--builtin">
							{#each visibleBuiltinTools as tool (tool.name)}
								<BuiltinToolCard {tool} onToggle={handleToolToggle} />
							{/each}
						</div>
					{/if}
				</section>
			{:else if activeTab === 'mcp'}
				<section class="resource-panel motion-surface-enter" aria-label="MCP 服务器">
					<WorkspaceSectionHeader
						title="MCP 服务器"
						description="连接外部工具服务，并查看当前连接状态。"
					>
						{#snippet children()}
							<div class="toolbar-actions toolbar-actions--paired">
								<RefreshButton
									width="equal"
									loading={mcpRefreshing}
									onclick={refreshMcpList}
								/>
								<MaterialButton
									variant="outlined"
									width="equal"
									label="添加"
									onclick={openAddDialog}
								/>
							</div>
						{/snippet}
					</WorkspaceSectionHeader>
					{@render resourceToolbar()}
					{#if mcpServers.length === 0}
						<AsyncState
							state="unconfigured"
							title="尚未配置 MCP 服务器"
							message="添加 MCP 服务器，为 Agent 扩展外部工具与资源。"
							actionLabel="添加 MCP 服务器"
							onAction={openAddDialog}
						/>
					{:else if visibleMcpServers.length === 0}
						<AsyncState
							title="没有匹配的 MCP 服务器"
							message="换一个关键词或清除状态筛选。"
						/>
					{:else}
						<div class="resource-list resource-list--managed">
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
					<WorkspaceSectionHeader
						title="技能"
						description="管理可被 Agent 调用的技能和执行脚本。"
					>
						{#snippet children()}
							<div class="toolbar-actions toolbar-actions--paired">
								<RefreshButton
									width="equal"
									loading={skillsRefreshing}
									onclick={refreshSkills}
								/>
								<MaterialButton
									variant="outlined"
									width="equal"
									label="打开"
									onclick={openFolder}
								/>
							</div>
						{/snippet}
					</WorkspaceSectionHeader>
					{@render resourceToolbar()}
					{#if skills.length === 0}
						<AsyncState
							state="unconfigured"
							title="暂无技能"
							message="将 SKILL.md 文件放入技能文件夹，然后点击刷新。"
							actionLabel="打开技能文件夹"
							onAction={openFolder}
						/>
					{:else if visibleSkills.length === 0}
						<AsyncState title="没有匹配的技能" message="换一个关键词或清除状态筛选。" />
					{:else}
						<div class="resource-list resource-list--managed">
							{#each visibleSkills as skill (skill.name)}
								<SkillCard {skill} onToggle={handleToggle} />
							{/each}
						</div>
					{/if}
				</section>
			{/if}
		</div>
	</div>
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
		margin-bottom: var(--md-sys-space-lg);
	}
	.resource-search {
		flex: 1 1 var(--md-comp-settings-control-width);
		min-width: 0;
	}
	.resource-filter-controls {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.resource-filter-controls :global(.md-select-container) {
		flex: 0 1 auto;
	}
	:global(.resource-count) {
		flex: 0 0 auto;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.resource-panel {
		display: flex;
		flex-direction: column;
		gap: 0;
		min-width: 0;
	}
	.resource-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 360px), 1fr));
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
		.toolbar-actions--paired {
			width: 100%;
		}
	}
	@media (min-width: 840px) {
		.tools-page {
			max-width: none;
		}
		.resource-list--builtin {
			grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
			align-items: start;
			gap: var(--md-sys-space-md);
		}
		.resource-list--managed {
			grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
			align-items: start;
			gap: var(--md-sys-space-md);
		}
		.resource-toolbar {
			position: sticky;
			top: var(--md-sys-space-lg);
			z-index: 2;
			box-shadow: var(--md-sys-elevation-1);
		}
	}
</style>
