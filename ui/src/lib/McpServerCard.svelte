<script>
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { server, onToggle, onEdit, onRemove, onReconnect } = $props();
	let expanded = $state(false);
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

	function toggleExpand() {
		expanded = !expanded;
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

	let ctxMenu = $state({ open: false, x: 0, y: 0 });

	/** @param {MouseEvent} e */
	function handleContextMenu(e) {
		e.preventDefault();
		e.stopPropagation();
		ctxMenu = { open: true, x: e.clientX, y: e.clientY };
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0 };
	}

	let ctxMenuItems = $derived.by(() => {
		const items = [];
		items.push(
			server.enabled
				? { id: 'disable', label: '禁用', icon: 'power', action: () => onToggle?.(server.name, false) }
				: { id: 'enable', label: '启用', icon: 'power', action: () => onToggle?.(server.name, true) },
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

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="server-card" class:expanded oncontextmenu={handleContextMenu}>
	<div class="card-header" onclick={toggleExpand} role="button" tabindex="0" onkeydown={(e) => e.key === 'Enter' && toggleExpand()}>
		<div class="card-info">
			<div class="card-name">
				<span class="card-name-text">{server.name}</span>
				<span class="tool-count">{server.tools?.length || 0} tools</span>
			</div>
			<div class="card-meta">
				<span class="transport-badge">{server.transport || 'stdio'}</span>
				{#if server.url}
					<span class="endpoint">{server.url}</span>
				{/if}
				<span
					class="enabled-badge"
					class:enabled={server.enabled}
					class:disabled={!server.enabled}
				>
					{server.enabled ? 'Enabled' : 'Disabled'}
				</span>
				<span class="status-badge" class:connected={isConnected()} class:offline={isOffline()} class:connecting={isConnecting()}>
					{statusLabel(server.status)}
				</span>
				{#if server.last_seen_at}
					<span class="last-seen">Last seen: {new Date(server.last_seen_at * 1000).toLocaleTimeString()}</span>
				{/if}
			</div>
			{#if server.last_error}
				<div class="error-msg">{server.last_error}</div>
			{/if}
		</div>
		<div class="card-actions" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="presentation">
			<MaterialSwitch checked={server.enabled} onChange={handleToggle} />
			<MaterialIconButton
				variant="primary"
				label="Refresh"
				onclick={handleReconnect}
				disabled={refreshing}
			>
				<svg
					class:spin={refreshing}
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<polyline points="23 4 23 10 17 10" />
					<path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
				</svg>
			</MaterialIconButton>
			<MaterialIconButton label="Edit" onclick={() => onEdit?.(server)}>✎</MaterialIconButton>
			<MaterialIconButton variant="danger" label="Remove" onclick={() => onRemove?.(server.name)}>✕</MaterialIconButton>
		</div>
	</div>
	{#if expanded}
		<div class="card-body">
			<h4>Tools</h4>
			{#if server.tools && server.tools.length > 0}
				<div class="tool-list">
					{#each server.tools as tool}
						<div class="tool-item">
							<div class="tool-item-name">{tool.name}</div>
							<div class="tool-item-desc">{tool.description || 'No description'}</div>
							{#if tool.input_schema && Object.keys(tool.input_schema).length > 0}
								<details class="schema-details">
									<summary>Input Schema</summary>
									<pre>{JSON.stringify(tool.input_schema, null, 2)}</pre>
								</details>
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
		</div>
	{/if}

	<ContextMenu
		open={ctxMenu.open}
		x={ctxMenu.x}
		y={ctxMenu.y}
		items={ctxMenuItems}
		onClose={closeCtxMenu}
	/>
</div>

<style>
	.server-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		margin-bottom: var(--md-sys-space-sm);
		overflow: hidden;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.server-card:hover {
		border-color: var(--md-sys-color-outline);
	}
	.server-card.expanded {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-elevation-1);
	}
	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
		cursor: pointer;
		user-select: none;
	}
	.card-info {
		flex: 1;
		min-width: 0;
	}
	.card-name {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		font-size: 15px;
		font-weight: 700;
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
		font-size: 11px;
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
		font-size: 11px;
	}
	.status-badge {
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 700;
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.enabled-badge {
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 700;
	}
	.enabled-badge.enabled {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.enabled-badge.disabled {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.status-badge.connected {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.status-badge.offline {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.status-badge.connecting {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}
	.tool-count {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: 1px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-size: 11px;
		font-weight: 600;
		white-space: nowrap;
	}
	.last-seen {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.error-msg {
		margin-top: var(--md-sys-space-xs);
		font-size: 12px;
		color: var(--md-sys-color-error);
	}
	.card-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
		align-items: center;
	}
	.card-body {
		padding: 0 var(--md-sys-space-xl) var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.card-body h4 {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 1px;
		margin: var(--md-sys-space-md) 0 var(--md-sys-space-sm);
		font-weight: 700;
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
		font-size: 13px;
		font-weight: 700;
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-2xs);
	}
	.tool-item-desc {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.schema-details {
		margin-top: var(--md-sys-space-sm);
		font-size: 11px;
	}
	.schema-details summary {
		color: var(--md-sys-color-on-surface-variant);
		cursor: pointer;
		user-select: none;
	}
	.schema-details pre {
		margin-top: var(--md-sys-space-xs);
		padding: var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-highest);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		overflow-x: auto;
		max-height: 200px;
	}
	.no-tools {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
		font-size: 12px;
		padding: var(--md-sys-space-sm) 0;
	}
	.diag-msg {
		color: var(--md-sys-color-warning, #b58900);
		font-size: 12px;
		line-height: 1.45;
		background: var(--md-sys-color-surface-container-high, rgba(0, 0, 0, 0.06));
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		white-space: pre-wrap;
		word-break: break-word;
	}
	.card-actions svg.spin {
		animation: md-icon-spin 0.9s linear infinite;
	}
	@keyframes md-icon-spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}
</style>