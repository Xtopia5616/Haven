<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import { copyText } from '$lib/clipboard.js';

	let { tool, onToggle } = $props();
	let expanded = $state(false);

	function toggleExpand() {
		expanded = !expanded;
	}

	function handleToggle(checked) {
		onToggle?.(tool.name, checked);
	}

	let ctxMenu = $state({ open: false, x: 0, y: 0 });

	function handleContextMenu(e) {
		e.preventDefault();
		e.stopPropagation();
		ctxMenu = { open: true, x: e.clientX, y: e.clientY };
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0 };
	}

	let ctxMenuItems = $derived([
		{ id: 'copyName', label: '复制名称', icon: 'copy', action: () => copyText(tool.name, '名称') },
		{
			id: 'copySchema',
			label: '复制 Schema',
			icon: 'copy',
			action: () => copyText(JSON.stringify(tool.schema, null, 2), 'Schema'),
		},
		tool.enabled
			? { id: 'disable', label: '禁用', icon: 'power', action: () => onToggle?.(tool.name, false) }
			: { id: 'enable', label: '启用', icon: 'power', action: () => onToggle?.(tool.name, true) },
	]);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="tool-card" class:expanded oncontextmenu={handleContextMenu}>
	<div
		class="card-header"
		onclick={toggleExpand}
		role="button"
		tabindex="0"
		onkeydown={(e) => e.key === 'Enter' && toggleExpand()}
	>
		<div class="card-info">
			<div class="card-name">{tool.name}</div>
			<div class="card-meta">
				<span class="risk-badge risk-{tool.risk}">Risk: {tool.risk}</span>
				<span
					class="enabled-badge"
					class:enabled={tool.enabled}
					class:disabled={!tool.enabled}
				>
					{tool.enabled ? 'Enabled' : 'Disabled'}
				</span>
			</div>
		</div>
		<div
			class="card-actions"
			onclick={(e) => e.stopPropagation()}
			onkeydown={() => {}}
			role="presentation"
		>
			<MaterialSwitch checked={tool.enabled} onChange={handleToggle} />
		</div>
	</div>
	{#if expanded}
		<div class="card-body">
			<p class="desc">{tool.desc || 'No description'}</p>
			{#if tool.schema && Object.keys(tool.schema).length > 0}
				<h4>Input Schema</h4>
				<pre>{JSON.stringify(tool.schema, null, 2)}</pre>
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
	.tool-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		margin-bottom: var(--md-sys-space-sm);
		overflow: hidden;
		transition:
			border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.tool-card:hover {
		border-color: var(--md-sys-color-outline);
	}
	.tool-card.expanded {
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
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-xs);
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
	.risk-badge,
	.enabled-badge {
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 700;
	}
	.risk-badge.risk-safe {
		background: var(--md-sys-color-primary-container, #d2e3fc);
		color: var(--md-sys-color-on-primary-container, #001d36);
	}
	.risk-badge.risk-low {
		background: var(--md-sys-color-tertiary-container, #cbe9f0);
		color: var(--md-sys-color-on-tertiary-container, #001f25);
	}
	.risk-badge.risk-medium {
		background: var(--md-sys-color-secondary-container, #d9e3f3);
		color: var(--md-sys-color-on-secondary-container, #0e1d31);
	}
	.risk-badge.risk-high {
		background: #ffd9d4;
		color: #410002;
	}
	.risk-badge.risk-critical {
		background: #93000a;
		color: #ffffff;
	}
	.risk-badge.risk-unknown {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.enabled-badge.enabled {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.enabled-badge.disabled {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.card-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
		align-items: center;
		flex-shrink: 0;
		margin-left: var(--md-sys-space-md);
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
	.desc {
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-md) 0;
		line-height: 1.45;
	}
	.card-body pre {
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
</style>
