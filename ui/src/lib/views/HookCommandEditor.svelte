<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import type { HookCommand } from '$lib/contracts/generatedCommands.ts';
	import { hookMatches, hookValidationErrors } from '$lib/lifecycleHooksEditor.ts';

	let {
		hook,
		index,
		count,
		eventLabel,
		toolEvent,
		veto,
		disabled,
		onPatch,
		onMove,
		onRemove,
		onDuplicate,
	}: {
		hook: HookCommand;
		index: number;
		count: number;
		eventLabel: string;
		toolEvent: boolean;
		veto: boolean;
		disabled: boolean;
		onPatch: (patch: Partial<HookCommand>) => void;
		onMove: (offset: -1 | 1) => void;
		onRemove: () => void;
		onDuplicate: () => void;
	} = $props();
	let testTool = $state('shell');
	const errors = $derived(hookValidationErrors(hook));
</script>

<fieldset class="hook-command" class:paused={!hook.enabled} {disabled}>
	<legend>{eventLabel} · 命令 {index + 1}</legend>
	<div class="hook-command__actions">
		<label class="toggle"
			><input
				type="checkbox"
				checked={hook.enabled}
				onchange={(event) => onPatch({ enabled: event.currentTarget.checked })}
			/>启用</label
		>
		<MaterialButton variant="text" label="复制" disabled={count >= 8} onclick={onDuplicate} />
		<MaterialButton
			variant="text"
			label="上移"
			disabled={index === 0}
			onclick={() => onMove(-1)}
		/>
		<MaterialButton
			variant="text"
			label="下移"
			disabled={index === count - 1}
			onclick={() => onMove(1)}
		/>
		<MaterialButton variant="text" label="删除" onclick={onRemove} />
	</div>
	<label
		><span>可执行文件</span><input
			class="md-input"
			value={hook.command}
			placeholder="例如：pwsh.exe"
			autocomplete="off"
			oninput={(event) => onPatch({ command: event.currentTarget.value })}
		/></label
	>
	<label
		><span>参数（每行一个）</span><textarea
			class="md-input hook-args"
			value={hook.args.join('\n')}
			placeholder={'-NoProfile\n-File\nscripts/hook.ps1'}
			oninput={(event) =>
				onPatch({
					args: event.currentTarget.value
						? event.currentTarget.value.replace(/\r\n/g, '\n').split('\n')
						: [],
				})}></textarea></label
	>
	<p>参数按字面传递，无需额外引号。相对脚本路径以当前执行工作区为基准。</p>
	<label
		><span>工具 matcher</span><input
			class="md-input"
			value={hook.matcher ?? ''}
			placeholder="留空或 * 匹配全部；例如 mcp_*.search"
			autocomplete="off"
			oninput={(event) => onPatch({ matcher: event.currentTarget.value || null })}
		/></label
	>
	{#if toolEvent}
		<label
			><span>测试工具名（仅预览）</span><input
				class="md-input"
				bind:value={testTool}
				maxlength="256"
			/></label
		>
		<p class="motion-surface-enter" role="status">
			{!hook.enabled
				? '已停用，不会执行'
				: hookMatches(hook, testTool)
					? '匹配此工具'
					: '不匹配此工具'} · 区分大小写，只有 * 是通配符。
		</p>
	{:else if hook.matcher !== null && hook.matcher !== '*'}
		<p class="warning motion-surface-enter" role="status">
			此事件没有工具名，当前 matcher 会使这条 Hook 永远不触发。清空 matcher 或设为 *。
		</p>
	{/if}
	{#if veto}<p class="motion-surface-enter">
			stdout 为空表示允许；也可返回 &#123;"decision":"allow"&#125; 或
			&#123;"decision":"deny","reason":"…"&#125;。调试信息写 stderr。
		</p>{/if}
	{#if errors.length}
		<ul class="errors motion-list" aria-label="命令校验错误">
			{#each errors as error}<li>{error}</li>{/each}
		</ul>
	{/if}
</fieldset>

<style>
	.hook-command {
		display: grid;
		gap: 0.65rem;
		margin: 0.8rem 0 0;
		min-width: 0;
		padding: 0.9rem;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 0.75rem;
		background: var(--md-sys-color-surface-container-low);
	}
	.paused {
		border-style: dashed;
	}
	legend {
		font-size: 0.875rem;
	}
	.hook-command__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.25rem;
	}
	label {
		display: grid;
		gap: 0.35rem;
		font-size: 0.875rem;
	}
	.toggle {
		display: flex;
		align-items: center;
		margin-right: auto;
	}
	.hook-args {
		min-height: 5rem;
		resize: vertical;
		font-family: var(--font-mono, monospace);
	}
	p {
		margin: 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.8125rem;
		overflow-wrap: anywhere;
	}
	.errors,
	.warning {
		color: var(--md-sys-color-error);
		font-size: 0.8125rem;
	}
	.errors {
		margin: 0;
		padding-left: 1.2rem;
	}
</style>
