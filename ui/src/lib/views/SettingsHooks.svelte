<script lang="ts">
	import { onMount } from 'svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import {
		getLifecycleHooksSettings,
		saveLifecycleHooks,
		trustHookWorkspace,
		untrustHookWorkspace,
	} from '$lib/settingsCommands.ts';
	import type {
		HookCommand,
		LifecycleHooksConfig,
		LifecycleHookEventDiagnostics,
		TauriCommandResponse,
	} from '$lib/contracts/generatedCommands.ts';

	type HookSettings = TauriCommandResponse<'get_lifecycle_hooks_settings'>;
	type HookListKey = keyof LifecycleHooksConfig;
	const MAX_HOOKS_PER_EVENT = 8;
	type DraftState = { dirty: boolean; save: () => Promise<boolean>; discard: () => void };
	interface Props {
		active: boolean;
		onDraftState: (draft: DraftState) => void;
	}

	let { active, onDraftState }: Props = $props();

	const EVENTS: Array<{ key: HookListKey; label: string; description: string; veto: boolean }> = [
		{
			key: 'pre_tool_use',
			label: 'PreToolUse',
			description: '工具执行前运行；脚本可返回 allow 或 deny。任何失败都会阻止该工具。',
			veto: true,
		},
		{
			key: 'post_tool_use',
			label: 'PostToolUse',
			description: '工具完成后运行；脚本失败只记录诊断，不改变工具结果。',
			veto: false,
		},
		{
			key: 'session_start',
			label: 'SessionStart',
			description: 'Haven 进程中的会话 actor 首次开始执行时运行一次。',
			veto: false,
		},
		{
			key: 'run_start',
			label: 'RunStart',
			description: '每次 Agent 执行开始时运行。',
			veto: false,
		},
		{
			key: 'run_end',
			label: 'RunEnd',
			description: '每次 Agent 执行退出时运行；包含完成、暂停、取消或错误结果。',
			veto: false,
		},
		{
			key: 'pre_compact',
			label: 'PreCompact',
			description: '上下文压缩前运行；可读取压缩前 token 估算。',
			veto: false,
		},
		{
			key: 'post_compact',
			label: 'PostCompact',
			description: '压缩结果成功写入会话后运行；包含压缩前后的 token 估算。',
			veto: false,
		},
	];

	const EMPTY_HOOKS = (): LifecycleHooksConfig => ({
		pre_tool_use: [],
		post_tool_use: [],
		session_start: [],
		run_start: [],
		run_end: [],
		pre_compact: [],
		post_compact: [],
	});

	let settings = $state<HookSettings | null>(null);
	let hooks = $state<LifecycleHooksConfig>(EMPTY_HOOKS());
	let savedHooks = $state<LifecycleHooksConfig | null>(null);
	let trustedWorkspaces = $state<string[]>([]);
	let diagnostics = $state<LifecycleHookEventDiagnostics[]>([]);
	let loading = $state(true);
	let refreshing = $state(false);
	let saving = $state(false);
	let errorMessage = $state('');
	const hooksDirty = $derived(
		savedHooks !== null && JSON.stringify(hooks) !== JSON.stringify(savedHooks),
	);

	const hookExecutionAllowed = $derived(
		settings?.sandbox_mode === 'full_access' && settings.network_policy === 'open',
	);

	async function reload() {
		loading = true;
		errorMessage = '';
		try {
			const current = await getLifecycleHooksSettings();
			settings = current;
			hooks = structuredClone(current.hooks);
			savedHooks = structuredClone(current.hooks);
			trustedWorkspaces = [...current.trusted_hook_workspaces];
			diagnostics = current.diagnostics;
		} catch (error) {
			errorMessage = '无法读取 Hook 设置。';
			reportError(error, { context: 'SettingsHooks', message: errorMessage, log: false });
		} finally {
			loading = false;
		}
	}

	async function refreshMetadata() {
		if (refreshing) return;
		refreshing = true;
		try {
			const current = await getLifecycleHooksSettings();
			settings = current;
			trustedWorkspaces = [...current.trusted_hook_workspaces];
			diagnostics = current.diagnostics;
		} catch (error) {
			reportError(error, { context: 'SettingsHooks', message: '刷新 Hook 状态失败', log: false });
		} finally {
			refreshing = false;
		}
	}

	function replaceHookList(key: HookListKey, value: HookCommand[]) {
		hooks = { ...hooks, [key]: value };
	}

	function addHook(key: HookListKey) {
		replaceHookList(key, [...hooks[key], { command: '', args: [], matcher: null }]);
	}

	function patchHook(key: HookListKey, index: number, patch: Partial<HookCommand>) {
		replaceHookList(
			key,
			hooks[key].map((hook, currentIndex) =>
				currentIndex === index ? { ...hook, ...patch } : hook,
			),
		);
	}

	function removeHook(key: HookListKey, index: number) {
		replaceHookList(key, hooks[key].filter((_, currentIndex) => currentIndex !== index));
	}

	function moveHook(key: HookListKey, index: number, offset: -1 | 1) {
		const nextIndex = index + offset;
		if (nextIndex < 0 || nextIndex >= hooks[key].length) return;
		const next = [...hooks[key]];
		[next[index], next[nextIndex]] = [next[nextIndex], next[index]];
		replaceHookList(key, next);
	}

	async function saveDraft(): Promise<boolean> {
		saving = true;
		errorMessage = '';
		try {
			await saveLifecycleHooks(hooks);
			savedHooks = structuredClone(hooks);
			addNotification('Lifecycle Hooks 已保存并立即生效', 'success');
			await refreshMetadata();
			return true;
		} catch (error) {
			errorMessage = '保存失败。请检查命令路径、参数数量和 matcher 格式。';
			reportError(error, { context: 'SettingsHooks', message: errorMessage, log: false });
			return false;
		} finally {
			saving = false;
		}
	}

	function discardDraft() {
		if (savedHooks) hooks = structuredClone(savedHooks);
		errorMessage = '';
	}

	async function trustWorkspace() {
		try {
			const path = await trustHookWorkspace();
			if (path) {
				addNotification('项目 Hook 信任目录已更新', 'success');
				await refreshMetadata();
			}
		} catch (error) {
			reportError(error, {
				context: 'SettingsHooks',
				message: '无法信任所选目录',
				log: false,
			});
		}
	}

	async function untrustWorkspace(path: string) {
		try {
			await untrustHookWorkspace(path);
			addNotification('项目 Hook 信任已撤销', 'success');
			await refreshMetadata();
		} catch (error) {
			reportError(error, {
				context: 'SettingsHooks',
				message: '撤销项目 Hook 信任失败',
				log: false,
			});
		}
	}

	function outcomeLabel(outcome: LifecycleHookEventDiagnostics['last_outcome']): string {
		if (!outcome) return '尚未触发';
		return {
			no_hooks: '没有匹配的 Hook',
			succeeded: '成功',
			denied: '已拒绝工具',
			failed_open: 'Hook 失败',
		}[outcome];
	}

	$effect(() => {
		onDraftState({ dirty: hooksDirty, save: saveDraft, discard: discardDraft });
	});

	$effect(() => {
		if (!active) return;
		const timer = setInterval(() => void refreshMetadata(), 5000);
		return () => clearInterval(timer);
	});

	onMount(() => void reload());
</script>

<div class="settings-hooks">
	<SettingsSection
		title="执行边界"
		description="Hook 命令继承 Haven 当前安全策略；只有 FullAccess 与开放网络同时启用时才会启动外部进程。"
	>
		<div class:allowed={hookExecutionAllowed} class="hook-policy" role="status">
			<strong>{hookExecutionAllowed ? '当前策略允许 Hook 运行' : '当前策略会阻止 Hook 运行'}</strong>
			<span>
				{#if hookExecutionAllowed}
					命令仍通过 Haven 的进程授权与进程树清理边界启动。
				{:else}
					PreToolUse 会 fail-closed 并阻止工具；通知型事件会跳过 Hook 并记录诊断。
				{/if}
			</span>
		</div>
	</SettingsSection>

	<SettingsSection
		title="用户级 Hooks"
		description="命令按原样启动，不经过 shell 展开；参数逐项传递。保存后立即生效。"
	>
		{#if loading}
			<p class="hooks-muted">正在读取 Hook 配置…</p>
		{:else if errorMessage}
			<p class="hooks-error" role="alert">{errorMessage}</p>
			<MaterialButton variant="outlined" label="重试" onclick={() => void reload()} />
		{:else}
			{#each EVENTS as event (event.key)}
				<section class="hook-event" aria-labelledby={`hook-event-${event.key}`}>
					<header class="hook-event__header">
						<div>
						<h3 id={`hook-event-${event.key}`}>{event.label}</h3>
						<p>{event.description}</p>
					</div>
						<MaterialButton
							variant="text"
							label="添加 Hook"
							disabled={hooks[event.key].length >= MAX_HOOKS_PER_EVENT}
							onclick={() => addHook(event.key)}
						/>
					</header>
					{#if hooks[event.key].length === 0}
						<p class="hooks-muted">尚未配置</p>
					{/if}
					{#each hooks[event.key] as hook, index (`${event.key}-${index}`)}
						<div class="hook-command">
							<div class="hook-command__title">
								<strong>命令 {index + 1}</strong>
								<div class="hook-command__actions">
									<MaterialButton
									variant="text"
									label="上移"
									disabled={index === 0}
									onclick={() => moveHook(event.key, index, -1)}
								/>
									<MaterialButton
									variant="text"
									label="下移"
									disabled={index === hooks[event.key].length - 1}
									onclick={() => moveHook(event.key, index, 1)}
								/>
								<MaterialButton
									variant="text"
									label="删除"
									onclick={() => removeHook(event.key, index)}
								/>
								</div>
							</div>
							<label>
								<span>可执行文件</span>
								<input
									class="md-input"
									value={hook.command}
									placeholder="例如：pwsh.exe"
									autocomplete="off"
									oninput={(inputEvent) => patchHook(event.key, index, { command: inputEvent.currentTarget.value })}
								/>
							</label>
							<label>
								<span>参数（每行一个）</span>
								<textarea
									class="md-input hook-args"
									value={hook.args.join('\n')}
									placeholder={'-NoProfile\n-File\nscripts/hook.ps1'}
									oninput={(inputEvent) =>
										patchHook(event.key, index, {
											args: inputEvent.currentTarget.value ? inputEvent.currentTarget.value.split('\n') : [],
										})}
								></textarea>
							</label>
							<label>
								<span>工具 matcher</span>
								<input
									class="md-input"
									value={hook.matcher ?? ''}
									placeholder="留空匹配所有工具；例如 mcp_*.search"
									autocomplete="off"
									oninput={(inputEvent) =>
										patchHook(event.key, index, {
											matcher: inputEvent.currentTarget.value || null,
										})}
								/>
							</label>
							{#if event.veto}
								<p class="hooks-note">非空 stdout 应为 JSON：&#123;"decision":"allow"&#125; 或 &#123;"decision":"deny","reason":"…"&#125;。</p>
							{/if}
						</div>
					{/each}
				</section>
			{/each}

			<div class="hooks-save-row">
				<p class="hooks-note">Hook 通过 stdin 接收 JSON。会话输入和工具结果可能含敏感内容，请只配置可信命令。</p>
				<MaterialButton
					variant="filled"
				label={saving ? '保存中…' : '保存 Hooks'}
					disabled={saving}
					onclick={() => void saveDraft()}
				/>
			</div>
		{/if}
	</SettingsSection>

	<SettingsSection
		title="项目 Hook 信任"
		description="信任目录后，目录中的 .haven/hooks.toml 才会加入配置。用户级 Hooks 先执行，项目级 Hooks 后执行。"
	>
		<div class="trust-actions">
			<p class="hooks-note">项目 Hook 文件可由仓库内容修改，因此仅对你明确选择的本地目录启用。</p>
			<MaterialButton
				variant="outlined"
				label="选择并信任目录"
				onclick={trustWorkspace}
			/>
		</div>
		{#if trustedWorkspaces.length === 0}
			<p class="hooks-muted">没有已信任的项目目录。</p>
		{:else}
			<ul class="trusted-workspaces">
				{#each trustedWorkspaces as path (path)}
					<li>
						<code>{path}</code>
						<MaterialButton
							variant="text"
							label="撤销"
							onclick={() => void untrustWorkspace(path)}
						/>
					</li>
				{/each}
			</ul>
		{/if}
	</SettingsSection>

	<SettingsSection
		title="运行诊断"
		description="仅保留当前进程中的事件计数、结果和静态失败码；不会记录命令参数、Hook 输入或输出。"
	>
		<div class="diagnostics-toolbar">
			<span>配置版本 {settings?.config_version ?? '—'}</span>
			<MaterialButton variant="text" label="刷新" onclick={() => void refreshMetadata()} disabled={loading || refreshing} />
		</div>
		{#if diagnostics.length === 0}
			<p class="hooks-muted">触发 Hook 后会在这里显示运行状态。</p>
		{:else}
			<div class="hook-diagnostics">
				{#each diagnostics as item (item.event)}
					<div class="hook-diagnostic-row">
						<strong>{item.event}</strong>
						<span>{outcomeLabel(item.last_outcome)}</span>
						<span>事件 {item.event_runs} 次 · 命令 {item.command_runs} 次</span>
						{#if item.failures || item.denials}
							<small>失败 {item.failures} · 拒绝 {item.denials} · {item.last_failure_code ?? ''}</small>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</SettingsSection>
</div>

<style>
	.settings-hooks {
		display: grid;
		gap: 1rem;
	}
	.hook-policy,
	.hook-event,
	.hook-command {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 0.75rem;
		padding: 0.9rem;
	}
	.hook-policy {
		display: grid;
		gap: 0.35rem;
		background: color-mix(in srgb, var(--md-sys-color-error-container) 38%, transparent);
	}
	.hook-policy.allowed {
		background: color-mix(in srgb, var(--md-sys-color-tertiary-container) 38%, transparent);
	}
	.hook-policy span,
	.hook-event p,
	.hooks-muted,
	.hooks-note,
	.hooks-error,
	.diagnostics-toolbar,
	.hook-diagnostic-row span,
	.hook-diagnostic-row small {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 0.875rem;
	}
	.hook-event + .hook-event {
		margin-top: 0.9rem;
	}
	.hook-event__header,
	.hook-command__title,
	.hook-command__actions,
	.trust-actions,
	.diagnostics-toolbar,
	.trusted-workspaces li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.hook-command__actions {
		margin-left: auto;
	}
	.hook-event__header h3 {
		margin: 0;
		font-size: 1rem;
	}
	.hook-event__header p {
		margin: 0.3rem 0 0;
	}
	.hook-command {
		display: grid;
		gap: 0.75rem;
		margin-top: 0.65rem;
		background: var(--md-sys-color-surface-container-low);
	}
	.hook-command label {
		display: grid;
		gap: 0.35rem;
		font-size: 0.875rem;
	}
	.hook-args {
		min-height: 5rem;
		resize: vertical;
		font-family: var(--font-mono, monospace);
	}
	.hooks-save-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		margin-top: 0.8rem;
	}
	.trusted-workspaces,
	.hook-diagnostics {
		display: grid;
		gap: 0.5rem;
		padding: 0;
		margin: 0.75rem 0 0;
		list-style: none;
	}
	.trusted-workspaces li,
	.hook-diagnostic-row {
		min-width: 0;
		padding: 0.65rem 0;
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.trusted-workspaces code {
		overflow-wrap: anywhere;
	}
	.hook-diagnostic-row {
		display: grid;
		grid-template-columns: minmax(7rem, 1fr) minmax(8rem, 1fr) minmax(14rem, 2fr) auto;
		align-items: center;
		gap: 0.5rem;
	}
	.hook-diagnostic-row small {
		grid-column: 1 / -1;
	}
	.hooks-error {
		color: var(--md-sys-color-error);
	}
	.hooks-note {
		margin: 0;
	}
	@media (max-width: 700px) {
		.hooks-save-row,
		.trust-actions {
			align-items: flex-start;
			flex-direction: column;
		}
		.hook-diagnostic-row {
			grid-template-columns: 1fr;
		}
		.hook-diagnostic-row small {
			grid-column: auto;
		}
	}
</style>
