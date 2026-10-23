<script lang="ts">
	let { isVisible = true }: { isVisible?: boolean } = $props();
	let mcpServers = $state<McpServerSnapshotView[]>([]);
	let skills = $state<SkillInfoView[]>([]);
	let builtinTools = $state<BuiltinToolEntry[]>([]);
	let builtinToolsLoading = $state(false);
	let builtinToolsFailed = $state(false);
	let mcpLoaded = $state(false);
	let mcpLoadFailed = $state(false);
	let skillsLoaded = $state(false);
	let skillsLoadFailed = $state(false);
	let activeTab = $state<ToolTabId>('builtin');
	let searchQuery = $state('');
	let enabledFilter = $state<'all' | 'enabled' | 'disabled'>('all');
	let mcpDialogOpen = $state(false);
	let mcpEditServer = $state<McpServerSnapshotView | null>(null);
	let mcpFeatureServer = $state<McpServerSnapshotView | null>(null);

	import { onMount, onDestroy } from 'svelte';
	import {
		addMcpServer,
		executeSkill,
		openSkillsDir,
		reconnectMcpServer,
		refreshMcpServers as refreshMcpServersCommand,
		refreshSkills as refreshSkillsCommand,
		removeMcpServer,
		setSkillEnabled,
		setToolEnabled,
		toggleMcpServer,
		updateMcpServer,
		listBuiltinToolManifests,
		listMcpServers,
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
	import McpFeatureDialog from '$lib/McpFeatureDialog.svelte';
	import BuiltinToolCard from '$lib/BuiltinToolCard.svelte';
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialTabs from '$lib/MaterialTabs.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import CountChip from '$lib/CountChip.svelte';
	import WorkspaceSearch from '$lib/WorkspaceSearch.svelte';
	import WorkspacePageHeader from '$lib/WorkspacePageHeader.svelte';
	import WorkspaceSectionHeader from '$lib/WorkspaceSectionHeader.svelte';
	import {
		builtinToolEntryFromManifest,
		filterBuiltinToolCard,
		groupBuiltinTools,
	} from '$lib/builtinToolPresentation.ts';
	import { setToolManifests } from '$lib/toolManifest.ts';
	import type { BuiltinToolEntry } from '$lib/builtinToolPresentation.ts';
	import type { McpServerConfigInput } from '$lib/contracts/generatedCommands.ts';
	import type { McpServerSnapshotView } from '$lib/contracts/mcp.ts';
	import type { SkillInfoView } from '$lib/contracts/skills.ts';

	interface ResourceFilterItem {
		name: string;
		enabled: boolean;
		desc?: string;
		description?: string;
		url?: string;
		transport?: string;
	}

	interface ToggleState {
		generation: number;
		latestEnabled: boolean;
		confirmedEnabled: boolean;
		pending: Promise<void> | null;
	}

	type ToggleScope = 'skill' | 'mcp' | 'builtin';

	const TOOL_TAB_IDS = ['builtin', 'mcp', 'skills'] as const;
	type ToolTabId = (typeof TOOL_TAB_IDS)[number];
	const TOOL_TAB_LABELS: Record<ToolTabId, string> = {
		builtin: '内置工具',
		mcp: 'MCP',
		skills: '技能',
	};

	let unlistenSkills: { dispose: () => void } | undefined;
	let unlistenMcp: { dispose: () => void } | undefined;
	let disposed = false;
	let mcpRefreshTimer: ReturnType<typeof setTimeout> | null = null;
	let mcpRefreshing = $state(false);
	let skillsRefreshing = $state(false);
	const toggleStates = new Map<string, ToggleState>();

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

	function openMcpFeatures(server: McpServerSnapshotView) {
		if (!server.enabled || server.status !== 'Connected' || !server.targetBinding) {
			reportError(new Error('MCP target is no longer connected'), {
				context: 'ToolsView',
				message: '该 MCP 服务器已断开或目标已变化，请刷新连接状态',
				log: false,
			});
			return;
		}
		mcpFeatureServer = server;
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
			? visibleBuiltinTools.reduce(
					(total, family) =>
						total +
						family.roots.reduce(
							(familyTotal, root) => familyTotal + root.operations.length,
							0,
						),
					0,
				)
			: activeTab === 'mcp'
				? visibleMcpServers.length
				: visibleSkills.length,
	);
	const activeResourceCountLabel = $derived(activeTab === 'builtin' ? '个操作' : '项');

	function scheduleMcpRefresh() {
		// Cold start emits Connecting+Connected per server; coalesce into one
		// list_mcp_servers round-trip instead of 2N full snapshots.
		if (mcpRefreshTimer) clearTimeout(mcpRefreshTimer);
		mcpRefreshTimer = setTimeout(() => {
			mcpRefreshTimer = null;
			refreshMcpServers(false);
		}, 120);
	}

	onMount(async () => {
		await refreshBuiltinTools();
		if (disposed) return;
		await refreshMcpServers();
		if (disposed) return;
		await refreshSkillList();
		if (disposed) return;
		const skillsListener = await registerAppListener(
			'skills:status_change',
			async () => {
				await refreshSkillList(false);
			},
			{ tag: 'tools' },
		);
		if (disposed) {
			skillsListener.dispose();
			return;
		}
		unlistenSkills = skillsListener;
		const mcpListener = await registerAppListener(
			'mcp:status_change',
			() => {
				scheduleMcpRefresh();
			},
			{ tag: 'tools' },
		);
		if (disposed) {
			mcpListener.dispose();
			return;
		}
		unlistenMcp = mcpListener;
	});

	onDestroy(() => {
		disposed = true;
		if (mcpRefreshTimer) clearTimeout(mcpRefreshTimer);
		unlistenSkills?.dispose();
		unlistenMcp?.dispose();
		toggleStates.clear();
	});

	async function refreshMcpServers(notifyOnError = false) {
		try {
			const result = await listMcpServers();
			if (disposed) return false;
			mcpServers = result || [];
			mcpLoadFailed = false;
			return true;
		} catch (error) {
			if (disposed) return false;
			mcpLoadFailed = true;
			reportError(error, {
				context: 'ToolsView',
				message: '加载 MCP 服务器失败',
				log: false,
				notify: notifyOnError,
			});
			return false;
		} finally {
			if (!disposed) mcpLoaded = true;
		}
	}

	async function refreshBuiltinTools() {
		if (builtinToolsLoading) return false;
		builtinToolsLoading = true;
		builtinToolsFailed = false;
		try {
			const result = await listBuiltinToolManifests();
			if (disposed) return false;
			const manifests = setToolManifests(result?.tools || []);
			builtinTools = manifests
				.map(builtinToolEntryFromManifest)
				.sort((a, b) => a.name.localeCompare(b.name));
			return true;
		} catch (error) {
			if (disposed) return false;
			builtinToolsFailed = true;
			reportError(error, {
				context: 'ToolsView',
				message: '加载内置工具失败',
				log: false,
				notify: false,
			});
			return false;
		} finally {
			if (!disposed) builtinToolsLoading = false;
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
					failed.length > 0 ? 'warning' : 'success',
					3000,
				);
			}
		} catch (e) {
			if (isAuthorizationConfirmationPending(e)) return;
			mcpLoadFailed = true;
			reportError(e, { context: 'ToolsView', message: '刷新 MCP 服务器失败', log: false });
		} finally {
			mcpRefreshing = false;
		}
	}

	async function refreshSkillList(notifyOnError = false) {
		try {
			const result = await listSkills();
			if (disposed) return false;
			skills = result || [];
			skillsLoadFailed = false;
			return true;
		} catch (error) {
			if (disposed) return false;
			skillsLoadFailed = true;
			reportError(error, {
				context: 'ToolsView',
				message: '加载技能列表失败',
				log: false,
				notify: notifyOnError,
			});
			return false;
		} finally {
			if (!disposed) skillsLoaded = true;
		}
	}

	// Serialize each resource's writes. The UI stays optimistic, while a failed
	// latest write is reconciled from the backend before falling back to the last
	// confirmed value. Older failures must never roll back a newer user intent.
	async function toggleItem<T extends { name: string; enabled: boolean }>(
		scope: ToggleScope,
		getList: () => T[],
		name: string,
		enabled: boolean,
		setList: (items: T[]) => void,
		update: () => Promise<void>,
		refresh: (() => Promise<boolean>) | null,
	) {
		const key = JSON.stringify([scope, name]);
		let state = toggleStates.get(key);
		if (!state) {
			const current = getList().find((item) => item.name === name)?.enabled ?? enabled;
			state = {
				generation: 0,
				latestEnabled: current,
				confirmedEnabled: current,
				pending: null,
			};
			toggleStates.set(key, state);
		}

		const currentState = state;
		const generation = ++currentState.generation;
		currentState.latestEnabled = enabled;
		const applyEnabled = (value: boolean) => {
			setList(
				getList().map((item) => (item.name === name ? { ...item, enabled: value } : item)),
			);
		};
		applyEnabled(enabled);

		const previous = currentState.pending;
		let task!: Promise<void>;
		task = (previous ?? Promise.resolve())
			.catch(() => undefined)
			.then(async () => {
				if (disposed || currentState.generation !== generation) return;
				try {
					await update();
					if (disposed) return;
					currentState.confirmedEnabled = enabled;
					if (refresh) await refresh();
					if (disposed) return;
					if (currentState.generation !== generation) {
						applyEnabled(currentState.latestEnabled);
						return;
					}
					addNotification(`${name} 已${enabled ? '启用' : '禁用'}`, 'success', 2000);
				} catch (error) {
					if (disposed || currentState.generation !== generation) return;
					const refreshed = refresh ? await refresh() : false;
					if (disposed) return;
					if (currentState.generation !== generation) {
						applyEnabled(currentState.latestEnabled);
						return;
					}
					if (!refreshed) applyEnabled(currentState.confirmedEnabled);
					reportError(error, {
						context: 'ToolsView',
						message: `切换 ${name} 失败`,
						log: false,
					});
				}
			})
			.finally(() => {
				if (currentState.pending !== task) return;
				currentState.pending = null;
				if (toggleStates.get(key) === currentState) toggleStates.delete(key);
			});
		currentState.pending = task;
		await task;
	}

	async function handleToggle(name: string, enabled: boolean) {
		await toggleItem(
			'skill',
			() => skills,
			name,
			enabled,
			(v) => (skills = v),
			() => setSkillEnabled({ name, enabled }),
			() => refreshSkillList(false),
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
			skillsLoadFailed = true;
			reportError(e, { context: 'ToolsView', message: '刷新技能失败', log: false });
		} finally {
			skillsRefreshing = false;
		}
	}

	async function openFolder() {
		try {
			await openSkillsDir();
			addNotification('已打开技能文件夹', 'info', 3000);
		} catch (e) {
			if (isAuthorizationConfirmationPending(e)) return;
			reportError(e, { context: 'ToolsView', message: '打开技能文件夹失败', log: false });
		}
	}

	function openAddDialog() {
		mcpEditServer = null;
		mcpDialogOpen = true;
	}

	function openEditDialog(server: McpServerSnapshotView) {
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
			const action = mcpEditServer ? '更新' : '添加';
			reportError(e, {
				context: 'ToolsView',
				message: `${action} MCP 服务器 ${config.name} 失败`,
				log: false,
			});
		}
	}

	async function handleRemove(name: string) {
		try {
			await removeMcpServer({ name });
			addNotification(`已移除 ${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			reportError(e, {
				context: 'ToolsView',
				message: `移除 MCP 服务器 ${name} 失败`,
				log: false,
			});
		}
	}

	async function handleReconnect(name: string) {
		try {
			await reconnectMcpServer({ name });
			addNotification(`刷新成功：${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			if (isAuthorizationConfirmationPending(e)) return;
			reportError(e, {
				context: 'ToolsView',
				message: `刷新 MCP 服务器 ${name} 连接失败`,
				log: false,
			});
		}
	}

	async function handleMcpToggle(name: string, enabled: boolean) {
		await toggleItem(
			'mcp',
			() => mcpServers,
			name,
			enabled,
			(v) => (mcpServers = v),
			() => toggleMcpServer({ name, enabled }),
			refreshMcpServers,
		);
	}

	async function handleToolToggle(name: string, enabled: boolean) {
		await toggleItem(
			'builtin',
			() => builtinTools,
			name,
			enabled,
			(v) => (builtinTools = v),
			() => setToolEnabled({ name, enabled }),
			null,
		);
	}

	const tabs = TOOL_TAB_IDS.map((id) => ({ id, label: TOOL_TAB_LABELS[id] }));
	function isToolTabId(value: string): value is ToolTabId {
		return TOOL_TAB_IDS.some((tabId) => tabId === value);
	}
	function selectToolTab(tabId: string) {
		if (isToolTabId(tabId)) activeTab = tabId;
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
					<div class="resource-search">
						<WorkspaceSearch
							label="搜索工具资源"
							value={searchQuery}
							placeholder="搜索名称、描述或地址"
							onInput={(value) => (searchQuery = value)}
						/>
					</div>
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
						label={activeResourceCountLabel}
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
							<RefreshButton
								width="compact"
								loading={builtinToolsLoading}
								title="刷新内置工具列表"
								onclick={refreshBuiltinTools}
							/>
							<MaterialButton
								variant="outlined"
								width="compact"
								label="重置熔断"
								onclick={resetToolCircuits}
							/>
						{/snippet}
					</WorkspaceSectionHeader>
					{@render resourceToolbar()}
					{#if builtinToolsFailed && builtinTools.length > 0}
						<AsyncState
							state="error"
							layout="compact"
							title="内置工具刷新失败"
							message="保留了上次加载的工具列表。检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshBuiltinTools}
						/>
					{/if}
					{#if builtinToolsLoading && builtinTools.length === 0}
						<AsyncState state="loading" title="正在加载内置工具" />
					{:else if builtinToolsFailed && builtinTools.length === 0}
						<AsyncState
							state="error"
							title="内置工具加载失败"
							message="检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshBuiltinTools}
						/>
					{:else if builtinTools.length === 0}
						<AsyncState
							title="没有注册的内置工具"
							message="当前版本没有可显示的内置工具。"
						/>
					{:else if visibleBuiltinTools.length === 0}
						<AsyncState
							title="没有匹配的内置工具"
							message="换一个关键词或清除状态筛选。"
						/>
					{:else}
						<div class="resource-list resource-list--builtin motion-list">
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
							<RefreshButton
								width="compact"
								loading={mcpRefreshing}
								onclick={refreshMcpList}
							/>
							<MaterialButton
								variant="outlined"
								width="compact"
								label="添加"
								onclick={openAddDialog}
							/>
						{/snippet}
					</WorkspaceSectionHeader>
					{#if mcpLoadFailed && mcpServers.length > 0}
						<AsyncState
							state="error"
							layout="compact"
							title="MCP 状态刷新失败"
							message="保留了上次加载的服务器列表。检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshMcpList}
						/>
					{/if}
					{@render resourceToolbar()}
					{#if !mcpLoaded}
						<AsyncState state="loading" title="正在加载 MCP 服务器" />
					{:else if mcpLoadFailed && mcpServers.length === 0}
						<AsyncState
							state="error"
							title="MCP 服务器加载失败"
							message="检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshMcpList}
						/>
					{:else if mcpServers.length === 0}
						<AsyncState
							state="unconfigured"
							title="尚未配置 MCP 服务器"
							message="添加 MCP 服务器，为 Agent 扩展外部工具与资源。"
							actionLabel="添加服务器"
							onAction={openAddDialog}
						/>
					{:else if visibleMcpServers.length === 0}
						<AsyncState
							title="没有匹配的 MCP 服务器"
							message="换一个关键词或清除状态筛选。"
						/>
					{:else}
						<div class="resource-list resource-list--managed motion-list">
							{#each visibleMcpServers as server (server.name)}
								<McpServerCard
									{server}
									onBrowseFeatures={openMcpFeatures}
									onEdit={openEditDialog}
									onRemove={handleRemove}
									onReconnect={handleReconnect}
									onToggle={handleMcpToggle}
								/>
							{/each}
						</div>
					{/if}
				</section>
			{:else if activeTab === 'skills'}
				<section class="resource-panel motion-surface-enter" aria-label="技能">
					<WorkspaceSectionHeader
						title="技能"
						description="管理可被 Agent 调用的技能和执行脚本。"
					>
						{#snippet children()}
							<RefreshButton
								width="compact"
								loading={skillsRefreshing}
								onclick={refreshSkills}
							/>
							<MaterialButton
								variant="outlined"
								width="compact"
								label="打开文件夹"
								ariaLabel="打开技能文件夹"
								onclick={openFolder}
							/>
						{/snippet}
					</WorkspaceSectionHeader>
					{#if skillsLoadFailed && skills.length > 0}
						<AsyncState
							state="error"
							layout="compact"
							title="技能列表刷新失败"
							message="保留了上次加载的技能列表。检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshSkills}
						/>
					{/if}
					{@render resourceToolbar()}
					{#if !skillsLoaded}
						<AsyncState state="loading" title="正在加载技能" />
					{:else if skillsLoadFailed && skills.length === 0}
						<AsyncState
							state="error"
							title="技能列表加载失败"
							message="检查应用连接后重试。"
							actionLabel="重试"
							onAction={refreshSkills}
						/>
					{:else if skills.length === 0}
						<AsyncState
							state="unconfigured"
							title="暂无技能"
							message="将 SKILL.md 文件放入技能文件夹，然后点击刷新。"
							actionLabel="打开文件夹"
							onAction={openFolder}
						/>
					{:else if visibleSkills.length === 0}
						<AsyncState title="没有匹配的技能" message="换一个关键词或清除状态筛选。" />
					{:else}
						<div class="resource-list resource-list--managed motion-list">
							{#each visibleSkills as skill (skill.name)}
								<SkillCard
									{skill}
									onPreview={executeSkill}
									onToggle={handleToggle}
								/>
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

{#if mcpFeatureServer}
	<McpFeatureDialog server={mcpFeatureServer} onClose={() => (mcpFeatureServer = null)} />
{/if}

<style>
	.tools-page {
		width: 100%;
		min-width: 0;
		max-width: var(--md-sys-content-max-width);
	}
	.tools-workspace-main {
		min-width: 0;
		container-type: inline-size;
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
	}
	@container (max-width: 640px) {
		.resource-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
		.resource-search {
			width: 100%;
			flex: 0 1 auto;
		}
		.resource-filter-controls {
			align-items: center;
			flex-wrap: wrap;
			width: 100%;
			gap: var(--md-sys-space-sm);
		}
		.resource-filter-controls :global(.md-select-container) {
			width: auto;
			flex: 1 1 160px;
		}
		:global(.resource-count) {
			align-self: flex-start;
		}
	}
</style>
