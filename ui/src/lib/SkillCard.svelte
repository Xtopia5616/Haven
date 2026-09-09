<script>
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import { formatError } from '$lib/formatError.ts';

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

	async function runPreview() {
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
			// Do not bypass AuthorizationEngine — preview must respect the same
			// confirmation / permanent-deny rules as agent-invoked skills.
			const result = await invoke('execute_skill', {
				name: skill.name,
				params,
			});
			previewResult = JSON.stringify(result, null, 2);
		} catch (err) {
			const msg = formatError(err);
			try {
				const parsed = JSON.parse(msg);
				if (parsed?.requires_confirmation) {
					previewResult =
						`${parsed.summary || '此技能执行需要确认'}（风险: ${parsed.risk_level || 'high'}）。` +
						'确认弹窗已打开，请在弹窗中选择执行范围。';
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
			<StatusBadge
				label={skill.enabled ? '已启用' : '已停用'}
				tone={skill.enabled ? 'success' : 'error'}
			/>
			{#if skill.has_script}
				<span class="script-badge">含脚本</span>
			{/if}
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch
			checked={skill.enabled}
			ariaLabel={`切换技能 ${skill.name}`}
			onChange={handleToggle}
		/>
	{/snippet}
	{#snippet children()}
		<p class="desc">{skill.description || '暂无描述'}</p>

		<h4>技能路径</h4>
		<code class="path">{skill.root}</code>

		{#if skill.has_script}
			<h4>执行预览</h4>
			<div class="preview-row">
				<textarea
					class="preview-input"
					bind:value={previewParams}
					rows="3"
					placeholder={'{"key": "value"}'}
					onclick={(e) => e.stopPropagation()}
					autocomplete="off"></textarea>
				<MaterialButton
					variant="filled"
					label={running ? '运行中…' : '运行'}
					className="btn-preview"
					onclick={runPreview}
					disabled={running}
				/>
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
	:global(.md-btn.btn-preview) {
		align-self: stretch;
		min-width: 64px;
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
