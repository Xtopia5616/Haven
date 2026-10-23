<script lang="ts">
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ExpandableContextCard from '$lib/ExpandableContextCard.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import { formatError } from '$lib/formatError.ts';
	import type {
		ExecuteSkillRequest,
		SkillExecutionResponse,
	} from '$lib/contracts/tools.ts';
	import type { SkillInfoView } from '$lib/contracts/skills.ts';
	import type { ContextMenuItem } from '$lib/contextMenuStore.ts';

	interface Props {
		skill: SkillInfoView;
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
		...(!skill.manifestError && (skill.enabled || skill.hasScript)
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
				<MaterialBadge text={skill.version} />
			{/if}
			<MaterialBadge text={skill.language} variant="secondary" />
			<StatusBadge
				label={skill.manifestError
					? '清单无效'
					: skill.executable
						? '已启用'
						: skill.enabled
							? '配置已启用，当前不可执行'
							: '已停用'}
				tone={skill.executable ? 'success' : 'error'}
			/>
			{#if skill.manifestError}
				<MaterialBadge text="清单无法解析" variant="error" />
			{:else if skill.hasScript}
				<MaterialBadge text="含脚本" variant="primary" />
			{:else}
				<MaterialBadge text="缺少入口脚本，无法执行" variant="error" />
			{/if}
		</div>
	{/snippet}
	{#snippet actions()}
		<MaterialSwitch
			checked={skill.enabled}
			ariaLabel={`切换技能 ${skill.name}`}
			disabled={Boolean(skill.manifestError) || (!skill.hasScript && !skill.enabled)}
			onChange={handleToggle}
		/>
	{/snippet}
	{#snippet children()}
		<p class="expandable-context-card-description">{skill.description || '暂无描述'}</p>
		{#if skill.manifestError}
			<p class="skill-unavailable-reason" role="status">{skill.manifestError}</p>
		{:else if skill.hasScript && skill.unavailableReason}
			<p class="skill-unavailable-reason" role="status">{skill.unavailableReason}</p>
		{/if}

		{#if skill.hasScript}
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
