<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import { parseHooksImport } from '$lib/lifecycleHooksEditor.ts';
	import type { LifecycleHooksConfig } from '$lib/contracts/generatedCommands.ts';
	let {
		hooks,
		disabled,
		onImport,
	}: {
		hooks: LifecycleHooksConfig;
		disabled: boolean;
		onImport: (hooks: LifecycleHooksConfig) => void;
	} = $props();
	let text = $state('');
	let error = $state('');
	function importDraft() {
		try {
			onImport(parseHooksImport(text));
			error = '';
			text = '';
		} catch (failure) {
			error = failure instanceof Error ? failure.message : '无法导入配置。';
		}
	}
</script>

<details>
	<summary>导入 / 导出用户级配置</summary>
	<p>导入会替换当前草稿，保存后才会运行。请先检查可信命令与启用状态；项目目录信任不会导入。</p>
	<label
		><span>Hook JSON 配置</span><textarea
			class="md-input"
			bind:value={text}
			{disabled}
			spellcheck="false"
			placeholder={'{"run_start":[{"command":"node.exe","args":["scripts/hook.mjs"],"enabled":false}]}'}
		></textarea></label
	>
	<div class="actions">
		<MaterialButton
			variant="outlined"
			label="载入为草稿"
			disabled={disabled || !text.trim()}
			onclick={importDraft}
		/>
		<MaterialButton
			variant="text"
			label="生成导出文本"
			{disabled}
			onclick={() => {
				text = JSON.stringify(hooks, null, 2);
				error = '';
			}}
		/>
	</div>
	<p>导出包含命令路径和参数，分享前请检查其中的私人信息。</p>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
</details>

<style>
	details {
		border-top: 1px solid var(--md-sys-color-outline-variant);
		padding-top: 0.8rem;
		margin-top: 0.8rem;
	}
	summary {
		cursor: pointer;
	}
	label {
		display: grid;
		gap: 0.35rem;
		font-size: 0.875rem;
	}
	textarea {
		min-height: 10rem;
		resize: vertical;
		font-family: var(--font-mono, monospace);
	}
	p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.8125rem;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin-top: 0.5rem;
	}
	.error {
		color: var(--md-sys-color-error);
	}
</style>
