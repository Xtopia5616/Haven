<script>
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import RefreshButton from '$lib/RefreshButton.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { server, onToggle, onEdit, onRemove, onReconnect } = $props();
	let refreshing = $state(false);

	async function handleReconnect() {
		if (refreshing) return;
		refreshing = true;
		try {
			await onReconnect?.(server.name);
		} finally {
			refreshing = false;
		}
	}

	/** @param {boolean} checked */
	function handleToggle(checked) {
		onToggle?.(server.name, checked);
	}

	/** @param {any} status */
	function statusLabel(status) {
		if (typeof status === 'string') return status;
		if (status && typeof status === 'object') {
			if ('Connected' in status) return 'Connected';
			if ('Connecting' in status) return 'Connecting';
			if ('Disconnected' in status) return 'Disconnected';
			if ('Offline' in status) {
				const err = status.Offline?.error || '';
				return err ? `Offline: ${err}` : 'Offline';
			}
		}
		return 'Unknown';
	}

	function isConnected() {
		const s = server.status;
		return s === 'Connected' || (typeof s === 'object' && 'Connected' in s);
	}

	function isOffline() {
		const s = server.status;
		return s === 'Offline' || (typeof s === 'object' && 'Offline' in s);
	}

	function isConnecting() {
		const s = server.status;
		return s === 'Connecting' || (typeof s === 'object' && 'Connecting' in s);
	}

	function statusTone() {
		if (isConnected()) return 'success';
		if (isOffline()) return 'error';
		if (isConnecting()) return 'warning';
		return 'neutral';
	}

	let contextMenuItems = $derived.by(() => {
		const items = [];
		items.push(
			server.enabled
				? {
						id: 'disable',
						label: '禁用',
						icon: 'power',
						action: () => onToggle?.(server.name, false),
					}
				: {
						id: 'enable',
						label: '启用',
						icon: 'power',
						action: () => onToggle?.(server.name, true),
					},
		);
		items.push({
			id: 'reconnect',
			label: '刷新',
			icon: 'refresh',
			action: handleReconnect,
		});
		items.push({ id: 'edit', label: '编辑', icon: 'edit', action: () => onEdit?.(server) });
		items.push({
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(server.name, '名称'),
		});
		items.push({
			id: 'remove',
			label: '移除',
			icon: 'delete',
			danger: true,
			action: () => onRemove?.(server.name),
		});
		return items;
	});
</script>

<ExpandableContextCard cardKind="mcp-server" {contextMenuItems}>
	{#snippet header()}
		<div class="card-name">
			<span class="card-name-text">{server.name}</span>
			<span class="tool-count">{server.tools?.length || 0} tools</span>
		</div>
		<div class="card-meta">
			<span class="transport-badge">{server.transport || 'stdio'}</span>
			{#if server.url}
				<span class="endpoint">{server.url}</span>
			{/if}
			<StatusBadge
				label={server.enabled ? 'Enabled' : 'Disabled'}
				tone={server.enabled ? 'success' : 'neutral'}
				className="enabled-badge"
			/>
			<StatusBadge label={statusLabel(server.status)} tone={statusTone()} />
			{#if server.last_seen_at}
				<span class="last-seen"
					>Last seen: {new Date(server.last_seen_at * 1000).toLocaleTimeString()}</span
				>
			{/if}
		</div>
		{#if server.last_error}
			<div class="error-msg">{server.last_error}</div>
		{/if}
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch
			checked={server.enabled}
			ariaLabel={`切换 ${server.name}`}
			onChange={handleToggle}
		/>
		<RefreshButton
			compact
			iconOnly
			label="刷新"
			loadingLabel="刷新中…"
			loading={refreshing}
			title="刷新 MCP 连接"
			onclick={handleReconnect}
		/>
		<MaterialIconButton
			icon="edit"
			label="编辑"
			title="编辑 MCP 服务器"
			onclick={() => onEdit?.(server)}
		/>
		<MaterialIconButton
			variant="danger"
			icon="delete"
			label="移除"
			title="移除 MCP 服务器"
			onclick={() => onRemove?.(server.name)}
		/>
	{/snippet}
	{#snippet children()}
		<h4>Tools</h4>
		{#if server.tools && server.tools.length > 0}
			<div class="tool-list">
				{#each server.tools as tool}
					<div class="tool-item">
						<div class="tool-item-name">{tool.name}</div>
						<div class="tool-item-desc">{tool.description || 'No description'}</div>
						{#if tool.input_schema && Object.keys(tool.input_schema).length > 0}
							<div class="schema-details">
								<MaterialCollapsible>
									{#snippet header()}
										<span class="schema-label">Input Schema</span>
									{/snippet}
									<pre>{JSON.stringify(tool.input_schema, null, 2)}</pre>
								</MaterialCollapsible>
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{:else}
			<p class="no-tools">No tools available</p>
			{#if server.diagnostic}
				<p class="diag-msg">{server.diagnostic}</p>
			{/if}
		{/if}
	{/snippet}
</ExpandableContextCard>

<style>
	.card-name {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
	}
	.card-name-text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.card-meta {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		flex-wrap: wrap;
	}
	.transport-badge {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 600;
	}
	.endpoint {
		color: var(--md-sys-color-primary);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
	}
	.tool-count {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: 1px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
	}
	.last-seen {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.error-msg {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-error);
	}
	.tool-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.tool-item {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-md);
	}
	.tool-item-name {
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-2xs);
	}
	.tool-item-desc {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.schema-details {
		margin-top: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.schema-label {
		color: var(--md-sys-color-on-surface-variant);
	}
	.schema-details pre {
		margin-top: var(--md-sys-space-xs);
		padding: var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-highest);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
		overflow-x: auto;
		max-height: 200px;
	}
	.no-tools {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		padding: var(--md-sys-space-sm) 0;
	}
	.diag-msg {
		color: var(--md-sys-color-warning, #b58900);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		background: var(--md-sys-color-surface-container-high, rgba(0, 0, 0, 0.06));
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		white-space: pre-wrap;
		word-break: break-word;
	}
</style>
