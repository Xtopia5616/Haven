<script lang="ts">
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import { formatError } from '$lib/formatError.ts';
	import type {
		ExecuteSkillRequest,
		SkillExecutionResponse,
		SkillInfo,
	} from '$lib/contracts/tools.ts';
	import type { ContextMenuItem } from '$lib/contextMenu.ts';

	interface Props {
		skill: SkillInfo;
		onPreview: (request: ExecuteSkillRequest) => Promise<SkillExecutionResponse>;
		onToggle?: (name: string, enabled: boolean) => void | Promise<void>;
	}

	let { skill, onPreview, onToggle }: Props = $props();

	function handleToggle(checked: boolean) {
		onToggle?.(skill.name, checked);
	}

	let contextMenuItems: ContextMenuItem[] = $derived([
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
		...(!skill.manifest_error && (skill.enabled || skill.has_script)
			? [
					skill.enabled
						? {
								id: 'disable',
								label: '禁用',
								icon: 'power' as const,
								action: () => onToggle?.(skill.name, false),
							}
						: {
								id: 'enable',
								label: '启用',
								icon: 'power' as const,
								action: () => onToggle?.(skill.name, true),
							},
				]
			: []),
	]);

	let previewParams = $state('{}');
	let previewResult = $state<string | null>(null);
	let running = $state(false);

	async function runPreview() {
		if (running) return;
		running = true;
		previewResult = null;
		let params: unknown;
		try {
			params = JSON.parse(previewParams);
		} catch {
			previewResult = 'Invalid JSON params';
			running = false;
			return;
		}
		try {
			const result = await onPreview({
				name: skill.name,
				params,
			});
			previewResult = JSON.stringify(result, null, 2);
		} catch (err) {
			const msg = formatError(err);
			try {
				const parsed = JSON.parse(msg);
				// The app-shell confirmation dialog owns this request. Repeating its
				// summary and instructions inside the skill card creates a second,
				// stale-looking permission prompt.
				if (!parsed?.requires_confirmation) {
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
		<div class="expandable-context-card-meta">
			{#if skill.version}
				<span class="meta-badge">{skill.version}</span>
			{/if}
			<span class="meta-badge lang">{skill.language}</span>
			<StatusBadge
				label={skill.manifest_error
					? '清单无效'
					: skill.executable
						? '已启用'
						: skill.enabled
							? '配置已启用，当前不可执行'
							: '已停用'}
				tone={skill.executable ? 'success' : 'error'}
			/>
			{#if skill.manifest_error}
				<span class="script-badge script-badge--missing">清单无法解析</span>
			{:else if skill.has_script}
				<span class="script-badge">含脚本</span>
			{:else}
				<span class="script-badge script-badge--missing">缺少入口脚本，无法执行</span>
			{/if}
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch
			checked={skill.enabled}
			ariaLabel={`切换技能 ${skill.name}`}
			disabled={Boolean(skill.manifest_error) || (!skill.has_script && !skill.enabled)}
			onChange={handleToggle}
		/>
	{/snippet}
	{#snippet children()}
		<p class="expandable-context-card-description">{skill.description || '暂无描述'}</p>
		{#if skill.manifest_error}
			<p class="skill-unavailable-reason" role="status">{skill.manifest_error}</p>
		{:else if skill.has_script && skill.unavailable_reason}
			<p class="skill-unavailable-reason" role="status">{skill.unavailable_reason}</p>
		{/if}

		<h4>技能路径</h4>
		<code class="path">{skill.root}</code>

		{#if skill.has_script}
			<h4>执行预览</h4>
			<div class="preview-row">
				<textarea
					class="md-textarea md-textarea--code md-textarea--compact preview-input"
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
	.script-badge--missing {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.path {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		word-break: break-all;
	}
	.skill-unavailable-reason {
		color: var(--md-sys-color-error);
		font-size: var(--md-sys-typescale-body-small-size);
		margin: var(--md-sys-space-sm) 0;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.preview-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: flex-start;
	}
	.preview-input {
		flex: 1;
	}
	:global(.md-btn.btn-preview) {
		align-self: stretch;
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
