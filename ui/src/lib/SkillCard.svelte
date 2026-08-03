<script>
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';

	let { skill, onToggle } = $props();
	let expanded = $state(false);

	function toggleExpand() {
		expanded = !expanded;
	}

	function handleToggle(e) {
		e.stopPropagation();
		onToggle?.(skill.name, !skill.enabled);
	}

	let previewParams = $state('{}');
	let previewResult = $state(null);
	let running = $state(false);

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
			const { invoke } = await import('$lib/tauri.js');
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

<div class="skill-card" class:expanded>
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
			<MaterialIconButton
				variant={skill.enabled ? 'primary' : 'default'}
				label={skill.enabled ? 'Disable' : 'Enable'}
				onclick={handleToggle}
			>
				{skill.enabled ? '◐' : '○'}
			</MaterialIconButton>
		</div>
	</div>
	{#if expanded}
		<div class="card-body">
			<p class="desc">{skill.description || 'No description'}</p>

			<h4>Allowed Tools</h4>
			{#if skill.allowed_tools && skill.allowed_tools.length > 0}
				<div class="tool-list">
					{#each skill.allowed_tools as tool}
						<div class="tool-item">{tool}</div>
					{/each}
				</div>
			{:else}
				<p class="empty-note">All tools allowed</p>
			{/if}

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
	.tool-list {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
	}
	.tool-item {
		font-size: 11px;
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		padding: var(--md-sys-space-2xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-extra-small);
		font-weight: 600;
	}
	.empty-note {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
		font-style: italic;
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
