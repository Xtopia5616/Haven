<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { skill, onToggle } = $props();
	let expanded = $state(false);

	function toggleExpand() {
		expanded = !expanded;
	}

	/** @param {boolean} checked */
	function handleToggle(checked) {
		onToggle?.(skill.name, checked);
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

	let ctxMenuItems = $derived([
		{ id: 'copyName', label: '复制名称', icon: 'copy', action: () => copyText(skill.name, '名称') },
		{
			id: 'copyDesc',
			label: '复制描述',
			icon: 'copy',
			action: () => copyText(skill.description || '', '描述'),
		},
		skill.enabled
			? { id: 'disable', label: '禁用', icon: 'power', action: () => onToggle?.(skill.name, false) }
			: { id: 'enable', label: '启用', icon: 'power', action: () => onToggle?.(skill.name, true) },
	]);

	let previewParams = $state('{}');
	let previewResult = /** @type {string | null} */ ($state(null));
	let running = $state(false);

	/** @param {MouseEvent} e */
	async function runPreview(e) {
		e.stopPropagation();
		if (running) return;
		running = true;
		previewResult = null;
		let params;
		try {
			params = JSON.parse(previewParams);
		} catch {
			previewResult = 'Invalid JSON params';
			running = false;
			return;
		}
		try {
			const { invoke } = await import('$lib/tauri.ts');
			const result = await invoke('execute_skill', {
				name: skill.name,
				params,
				confirmed: true,
			});
			previewResult = JSON.stringify(result, null, 2);
		} catch (err) {
			previewResult = `Error: ${err}`;
		}
		running = false;
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="skill-card" class:expanded oncontextmenu={handleContextMenu}>
	<div
		class="card-header"
		onclick={toggleExpand}
		role="button"
		tabindex="0"
		onkeydown={(e) => e.key === 'Enter' && toggleExpand()}
	>
		<div class="card-info">
			<div class="card-name">{skill.name}</div>
			<div class="card-meta">
				{#if skill.version}
					<span class="meta-badge">{skill.version}</span>
				{/if}
				<span class="meta-badge lang">{skill.language}</span>
				<span class="status-badge" class:enabled={skill.enabled} class:disabled={!skill.enabled}>
					{skill.enabled ? 'Enabled' : 'Disabled'}
				</span>
				{#if skill.has_script}
					<span class="script-badge">script</span>
				{/if}
			</div>
		</div>
		<div
			class="card-actions"
			onclick={(e) => e.stopPropagation()}
			onkeydown={() => {}}
			role="presentation"
		>
			<MaterialSwitch checked={skill.enabled} onChange={handleToggle} />
		</div>
	</div>
	{#if expanded}
		<div class="card-body">
			<p class="desc">{skill.description || 'No description'}</p>

			<h4>Root</h4>
			<code class="path">{skill.root}</code>

			{#if skill.has_script}
				<h4>Execution Preview</h4>
				<div class="preview-row">
					<textarea
						class="preview-input"
						bind:value={previewParams}
						rows="3"
						placeholder={'{"key": "value"}'}
						onclick={(e) => e.stopPropagation()}
						autocomplete="off"
					></textarea>
					<button class="btn-preview" onclick={runPreview} disabled={running}>
						{running ? 'Running...' : 'Run'}
					</button>
				</div>
				{#if previewResult}
					<pre class="preview-result">{previewResult}</pre>
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
	.skill-card {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		margin-bottom: var(--md-sys-space-sm);
		overflow: hidden;
		transition:
			border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.skill-card:hover {
		border-color: var(--md-sys-color-outline);
	}
	.skill-card.expanded {
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
	.meta-badge {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 600;
	}
	.meta-badge.lang {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.status-badge {
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 700;
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.status-badge.enabled {
		background: var(--md-sys-color-success-container);
		color: var(--md-sys-color-on-success-container);
	}
	.status-badge.disabled {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.script-badge {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		padding: 2px var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-weight: 600;
	}
	.card-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
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
	.path {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		word-break: break-all;
	}
	.preview-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: flex-start;
	}
	.preview-input {
		flex: 1;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-small);
		color: var(--md-sys-color-on-surface);
		font-size: 12px;
		padding: var(--md-sys-space-sm);
		font-family: var(--md-sys-typescale-mono);
		resize: vertical;
		transition:
			border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.preview-input:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
	}
	.btn-preview {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		border: none;
		border-radius: var(--md-sys-shape-extra-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		font-size: 12px;
		cursor: pointer;
		font-weight: 600;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.btn-preview:hover {
		background: color-mix(in srgb, var(--md-sys-color-on-primary) 8%, var(--md-sys-color-primary));
	}
	.btn-preview:disabled {
		opacity: 0.38;
		cursor: not-allowed;
	}
	.preview-result {
		margin-top: var(--md-sys-space-sm);
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-extra-small);
		padding: var(--md-sys-space-sm);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
	}
</style>
