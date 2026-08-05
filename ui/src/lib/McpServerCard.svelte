<script>
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';

	let { server, onToggle, onEdit, onRemove, onReconnect } = $props();
	let expanded = $state(false);

	function toggleExpand() {
		expanded = !expanded;
	}

	function handleToggle(checked) {
		onToggle?.(server.name, checked);
	}

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
</script>

<div class="server-card" class:expanded>
	<div class="card-header" onclick={toggleExpand} role="button" tabindex="0" onkeydown={(e) => e.key === 'Enter' && toggleExpand()}>
		<div class="card-info">
			<div class="card-name">{server.name}</div>
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
				<span class="tool-count">{server.tools?.length || 0} tools</span>
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
			{#if isOffline()}
				<MaterialIconButton variant="primary" label="Reconnect" onclick={() => onReconnect?.(server.name)}>↻</MaterialIconButton>
			{/if}
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
			{/if}
		</div>
	{/if}
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
		font-size: 15px;
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
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
	.tool-count,
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
</style>