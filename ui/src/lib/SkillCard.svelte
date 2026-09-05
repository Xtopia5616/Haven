<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let { skill, onToggle } = $props();

	/** @param {boolean} checked */
	function handleToggle(checked) {
		onToggle?.(skill.name, checked);
	}

	let contextMenuItems = $derived([
		{
			id: 'copyName',
			label: '复制名称',
			icon: 'copy',
			action: () => copyText(skill.name, '名称'),
		},
		{
			id: 'copyDesc',
			label: '复制描述',
			icon: 'copy',
			action: () => copyText(skill.description || '', '描述'),
		},
		skill.enabled
			? {
					id: 'disable',
					label: '禁用',
					icon: 'power',
					action: () => onToggle?.(skill.name, false),
				}
			: {
					id: 'enable',
					label: '启用',
					icon: 'power',
					action: () => onToggle?.(skill.name, true),
				},
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
			// Do not bypass SafetyGateway — preview must respect the same
			// confirmation / permanent-deny rules as agent-invoked skills.
			const result = await invoke('execute_skill', {
				name: skill.name,
				params,
			});
			previewResult = JSON.stringify(result, null, 2);
		} catch (err) {
			const msg = String(err ?? '');
			try {
				const parsed = JSON.parse(msg);
				if (parsed?.requires_confirmation) {
					previewResult =
						`需要确认才能执行（风险: ${parsed.risk_level || 'high'}）。` +
						`请在对话中由 Agent 调用该技能，或在设置里将该技能加入永久允许。`;
				} else {
					previewResult = `Error: ${msg}`;
				}
			} catch {
				previewResult = `Error: ${msg}`;
			}
		}
		running = false;
	}
</script>

<ExpandableContextCard {contextMenuItems}>
	{#snippet header()}
		<div class="card-name">{skill.name}</div>
		<div class="card-meta">
			{#if skill.version}
				<span class="meta-badge">{skill.version}</span>
			{/if}
			<span class="meta-badge lang">{skill.language}</span>
			<span
				class="status-badge"
				class:enabled={skill.enabled}
				class:disabled={!skill.enabled}
			>
				{skill.enabled ? 'Enabled' : 'Disabled'}
			</span>
			{#if skill.has_script}
				<span class="script-badge">script</span>
			{/if}
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch checked={skill.enabled} onChange={handleToggle} />
	{/snippet}
	{#snippet children()}
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
					autocomplete="off"></textarea>
				<button class="btn-preview" onclick={runPreview} disabled={running}>
					{running ? 'Running...' : 'Run'}
				</button>
			</div>
			{#if previewResult}
				<pre class="preview-result">{previewResult}</pre>
			{/if}
		{/if}
	{/snippet}
</ExpandableContextCard>

<style>
	.card-name {
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
	}
	.card-meta {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
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
	.desc {
		font-size: var(--md-sys-typescale-body-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-md) 0;
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.path {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
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
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		padding: var(--md-sys-space-sm);
		font-family: var(--md-sys-typescale-mono);
		resize: vertical;
		transition: border-color var(--md-sys-motion-duration-short)
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
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		cursor: pointer;
		font-weight: 600;
		transition: background-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.btn-preview:hover {
		background: color-mix(
			in srgb,
			var(--md-sys-color-on-primary) 8%,
			var(--md-sys-color-primary)
		);
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
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
	}
</style>
