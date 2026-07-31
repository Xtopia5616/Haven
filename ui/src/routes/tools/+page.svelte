<script>
	let mcpServers = $state([]);
	let skills = $state([]);
	let builtinTools = $state([]);
	let activeTab = $state('builtin');
	let mcpDialogOpen = $state(false);
	let mcpEditServer = $state(null);

	import { onMount, onDestroy } from 'svelte';
	import { invoke, listen } from '$lib/tauri.js';
	import { addNotification } from '$lib/stores.js';
	import logger from '$lib/logger.js';
	import SkillCard from '$lib/SkillCard.svelte';
	import McpServerCard from '$lib/McpServerCard.svelte';
	import McpEditDialog from '$lib/McpEditDialog.svelte';

	let unlistenSkills;
	let unlistenMcp;

	onMount(async () => {
		try {
			const result = await invoke('get_tools');
			if (result && result.tools) {
				builtinTools = result.tools.map((t) => ({
					name: t.name || 'unknown',
					desc: t.description || '',
					risk: t.risk_level || 'unknown',
					schema: t.input_schema || {},
				}));
			}
		} catch {
			builtinTools = [];
			logger.warn('tools', 'get_tools error');
			addNotification('Failed to load tools', 'error', 3000);
		}
		await refreshMcpServers();
		await refreshSkillList();
		try {
			unlistenSkills = await listen('skills:status_change', async () => {
				await refreshSkillList();
			});
			unlistenMcp = await listen('mcp:status_change', async () => {
				await refreshMcpServers();
			});
		} catch (e) {
			logger.warn('tools', 'listen registration error', e);
		}
	});

	onDestroy(() => {
		unlistenSkills?.();
		unlistenMcp?.();
	});

	async function refreshMcpServers() {
		try {
			const result = await invoke('list_mcp_tools');
			mcpServers = result || [];
		} catch {
			mcpServers = [];
			logger.warn('tools', 'list_mcp_tools error');
		}
	}

	async function refreshMcpList() {
		await refreshMcpServers();
		addNotification('MCP servers refreshed', 'success', 2000);
	}

	async function refreshSkillList() {
		try {
			const result = await invoke('list_skills');
			skills = result || [];
		} catch {
			skills = [];
			logger.warn('tools', 'list_skills error');
		}
	}

	async function handleToggle(name, enabled) {
		const prev = skills.map((s) => ({ ...s }));
		skills = skills.map((s) => (s.name === name ? { ...s, enabled } : s));
		try {
			await invoke('set_skill_enabled', { name, enabled });
			addNotification(
				`${name} ${enabled ? 'enabled' : 'disabled'}`,
				'success',
				2000,
			);
		} catch {
			skills = prev;
			addNotification(`Failed to toggle ${name}`, 'error', 3000);
		}
	}

	async function refreshSkills() {
		try {
			await invoke('refresh_skills');
			await refreshSkillList();
			addNotification('Skills refreshed', 'success', 2000);
		} catch {
			addNotification('Failed to refresh skills', 'error', 3000);
		}
	}

	async function openFolder() {
		try {
			const path = await invoke('open_skills_dir');
			addNotification(`Opened: ${path}`, 'info', 3000);
		} catch {
			addNotification('Failed to open skills folder', 'error', 3000);
		}
	}

	function openAddDialog() {
		mcpEditServer = null;
		mcpDialogOpen = true;
	}

	function openEditDialog(server) {
		mcpEditServer = server;
		mcpDialogOpen = true;
	}

	function closeDialog() {
		mcpDialogOpen = false;
		mcpEditServer = null;
	}

	async function handleSave(config) {
		try {
			if (mcpEditServer) {
				await invoke('update_mcp_server', { name: mcpEditServer.name, config });
				addNotification(`Updated ${config.name}`, 'success', 2000);
			} else {
				await invoke('add_mcp_server', { config });
				addNotification(`Added ${config.name}`, 'success', 2000);
			}
			closeDialog();
			await refreshMcpServers();
		} catch (e) {
			addNotification(`Failed: ${e}`, 'error', 4000);
		}
	}

	async function handleRemove(name) {
		try {
			await invoke('remove_mcp_server', { name });
			addNotification(`Removed ${name}`, 'success', 2000);
			await refreshMcpServers();
		} catch (e) {
			addNotification(`Failed to remove: ${e}`, 'error', 3000);
		}
	}

	async function handleReconnect(name) {
		try {
			await invoke('reconnect_mcp', { name });
			addNotification(`Reconnecting ${name}...`, 'info', 2000);
			await refreshMcpServers();
		} catch (e) {
			addNotification(`Failed to reconnect: ${e}`, 'error', 3000);
		}
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
			<h2>Built-in Tools</h2>
			<div class="tool-grid">
				{#each builtinTools as tool}
					<div class="tool-card">
						<div class="tool-name">{tool.name}</div>
						<div class="tool-desc">{tool.desc}</div>
						<div class="tool-risk risk-{tool.risk}">Risk: {tool.risk}</div>
					</div>
				{/each}
			</div>
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
							onToggle={() => {}}
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
		max-width: 1000px;
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
	.tool-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: var(--md-sys-space-md);
	}
	.tool-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md);
		transition: box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.tool-card:hover {
		border-color: var(--md-sys-color-outline);
		box-shadow: var(--md-sys-elevation-1);
	}
	.tool-name {
		font-size: 15px;
		font-weight: 700;
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-desc {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-sm);
		line-height: 1.45;
	}
	.tool-risk {
		display: inline-flex;
		align-items: center;
		font-size: 11px;
		font-weight: 600;
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
	}
	.risk-safe {
		background: var(--md-sys-color-primary-container, #d2e3fc);
		color: var(--md-sys-color-on-primary-container, #001d36);
	}
	.risk-low {
		background: var(--md-sys-color-tertiary-container, #cbe9f0);
		color: var(--md-sys-color-on-tertiary-container, #001f25);
	}
	.risk-medium {
		background: var(--md-sys-color-secondary-container, #d9e3f3);
		color: var(--md-sys-color-on-secondary-container, #0e1d31);
	}
	.risk-high {
		background: #ffd9d4;
		color: #410002;
	}
	.risk-critical {
		background: #93000a;
		color: #ffffff;
	}
	.risk-unknown {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
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
