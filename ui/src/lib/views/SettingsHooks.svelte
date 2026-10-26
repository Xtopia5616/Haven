<script lang="ts">
	import { onMount } from 'svelte';
	import { fade } from 'svelte/transition';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import HookCommandEditor from './HookCommandEditor.svelte';
	import HookConfigTransfer from './HookConfigTransfer.svelte';
	import {
		HOOK_EVENTS as EVENTS,
		HOOK_TEMPLATES,
		MAX_HOOKS_PER_EVENT,
		emptyHooks,
		createHook,
		hookValidationErrors,
		hookFailureHelp,
		type HookListKey,
		type HookTemplateKey,
	} from '$lib/lifecycleHooksEditor.ts';
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
	type DraftState = { dirty: boolean; save: () => Promise<boolean>; discard: () => void };
	interface Props {
		active: boolean;
		onDraftState: (draft: DraftState) => void;
	}

	let { active, onDraftState }: Props = $props();

	let settings = $state<HookSettings | null>(null);
	let hooks = $state<LifecycleHooksConfig>(emptyHooks());
	let savedHooks = $state<LifecycleHooksConfig | null>(null);
	let trustedWorkspaces = $state<string[]>([]);
	let diagnostics = $state<LifecycleHookEventDiagnostics[]>([]);
	let loading = $state(true);
	let refreshing = $state(false);
	let saving = $state(false);
	let errorMessage = $state('');
	let eventFilter = $state<HookListKey | 'all'>('all');
	let template = $state<HookTemplateKey>('blank');
	const validationErrors = $derived(
		EVENTS.flatMap((event) =>
			hooks[event.key].flatMap((hook, index) =>
				hookValidationErrors(hook).map(
					(error) => `${event.label} · 命令 ${index + 1}：${error}`,
				),
			),
		),
	);
	const totalHooks = $derived(
		EVENTS.reduce((total, event) => total + hooks[event.key].length, 0),
	);
	const enabledHooks = $derived(
		EVENTS.reduce(
			(total, event) => total + hooks[event.key].filter((hook) => hook.enabled).length,
			0,
		),
	);
	const hooksDirty = $derived(
		savedHooks !== null && JSON.stringify(hooks) !== JSON.stringify(savedHooks),
	);

	const hookExecutionMode = $derived.by(() => {
		const policy = settings?.resource_policy;
		if (!policy) return 'unknown';
		if (policy.files.write_mode === 'unrestricted') {
			return policy.network.access === 'open' && policy.network.allowed_domains.length === 0
				? 'full_access'
				: 'blocked';
		}
		if (policy.files.write_mode === 'workspace' || policy.files.write_mode === 'roots') {
			return 'workspace_write';
		}
		return 'blocked';
	});
	const hookExecutionAllowed = $derived(
		hookExecutionMode === 'full_access' || hookExecutionMode === 'workspace_write',
	);

	async function reload() {
		loading = true;
		errorMessage = '';
		try {
			const current = await getLifecycleHooksSettings();
			const draft = structuredClone(current.hooks);
			const trusted = [...current.trusted_hook_workspaces];
			hooks = draft;
			savedHooks = structuredClone(current.hooks);
			trustedWorkspaces = trusted;
			diagnostics = current.diagnostics;
			settings = current;
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
			const trusted = [...current.trusted_hook_workspaces];
			trustedWorkspaces = trusted;
			diagnostics = current.diagnostics;
			settings = current;
		} catch (error) {
			reportError(error, {
				context: 'SettingsHooks',
				message: '刷新 Hook 状态失败',
				log: false,
			});
		} finally {
			refreshing = false;
		}
	}

	function replaceHookList(key: HookListKey, value: HookCommand[]) {
		hooks = { ...hooks, [key]: value };
	}

	function addHook(key: HookListKey) {
		if (hooks[key].length >= MAX_HOOKS_PER_EVENT || saving) return;
		replaceHookList(key, [...hooks[key], createHook(template)]);
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
		replaceHookList(
			key,
			hooks[key].filter((_, currentIndex) => currentIndex !== index),
		);
	}

	function moveHook(key: HookListKey, index: number, offset: -1 | 1) {
		const nextIndex = index + offset;
		if (nextIndex < 0 || nextIndex >= hooks[key].length) return;
		const next = [...hooks[key]];
		[next[index], next[nextIndex]] = [next[nextIndex], next[index]];
		replaceHookList(key, next);
	}

	async function saveDraft(): Promise<boolean> {
		if (saving || loading || !settings) return false;
		if (validationErrors.length) {
			errorMessage = '请修正标出的字段后保存，停用的命令也需要有效配置。';
			return false;
		}
		const submitted = $state.snapshot(hooks);
		saving = true;
		errorMessage = '';
		try {
			await saveLifecycleHooks(submitted);
			savedHooks = structuredClone(submitted);
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
		if (savedHooks) hooks = structuredClone($state.snapshot(savedHooks));
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

	function outcomeLabel(item: LifecycleHookEventDiagnostics): string {
		const outcome = item.last_outcome;
		if (!outcome) return '执行中';
		return {
			no_hooks: '没有匹配的 Hook',
			succeeded: '成功',
			denied:
				item.event === 'permission_request' ? '已阻止确认请求' : '已拒绝工具',
			failed_open:
				item.event === 'pre_tool_use' || item.event === 'permission_request'
					? '失败，已阻止操作'
					: '失败，主操作继续',
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
		description="FullAccess 下需要开放网络且没有域名白名单；WorkspaceWrite 下仅在 Windows 通过 MXC 启动并关闭网络；ReadOnly 下不会启动 Hook。"
	>
		<div class:allowed={hookExecutionAllowed} class="hook-policy" role="status">
			<strong
				>{!settings
					? '尚未读取执行策略'
					: hookExecutionMode === 'full_access'
						? 'FullAccess 允许 Hook 运行'
						: hookExecutionMode === 'workspace_write'
							? 'WorkspaceWrite 使用 MXC 启动 Hook'
							: '当前策略会阻止 Hook 运行'}</strong
			>
			<span>
				{#if hookExecutionMode === 'full_access'}
					命令在宿主用户权限下运行，并受 Haven 的进程授权与进程树清理约束。
				{:else if hookExecutionMode === 'workspace_write'}
					Windows MXC 按当前 Hook 文件范围限制读写且禁用网络。非 Windows 或 MXC 无法启动时，Hook 会被拒绝。
				{:else}
					匹配到的启用 PreToolUse 或 PermissionRequest 无法启动时会阻止操作；通知型事件会跳过 Hook 并记录诊断。
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
		{:else if !settings}
			<p class="hooks-error motion-surface-enter" role="alert">{errorMessage}</p>
			<MaterialButton variant="outlined" label="重试" onclick={() => void reload()} />
		{:else}
			{#if errorMessage}
				<p class="hooks-error motion-surface-enter" role="alert">{errorMessage}</p>
			{/if}
			<div class="hooks-toolbar">
				<label
					>事件筛选<select class="md-input" bind:value={eventFilter}
						><option value="all">全部事件</option>{#each EVENTS as event}<option
								value={event.key}>{event.label} ({hooks[event.key].length})</option
							>{/each}</select
					></label
				>
				<label
					>新增命令模板<select class="md-input" bind:value={template}
						>{#each Object.entries(HOOK_TEMPLATES) as [key, item]}<option value={key}
								>{item.label}</option
							>{/each}</select
					></label
				>
				<span role="status"
					>{enabledHooks} 条启用 / {totalHooks} 条配置{hooksDirty
						? ' · 有未保存改动'
						: ''}</span
				>
			</div>
			{#if template !== 'blank'}<p class="hooks-note motion-surface-enter">
					模板只填充启动参数。优先将脚本放在执行工作区；WorkspaceWrite 下，工作区外的脚本目录必须位于安全设置的系统与工具链只读目录范围内。
				</p>{/if}
			{#each EVENTS.filter((event) => eventFilter === 'all' || event.key === eventFilter) as event (event.key)}
				<section class="hook-event" aria-labelledby={`hook-event-${event.key}`}>
					<header class="hook-event__header">
						<div>
							<h3 id={`hook-event-${event.key}`}>{event.label}</h3>
							<p>{event.description}</p>
						</div>
						<MaterialButton
							variant="text"
							label="添加 Hook"
							disabled={saving || hooks[event.key].length >= MAX_HOOKS_PER_EVENT}
							onclick={() => addHook(event.key)}
						/>
					</header>
					<p class="hooks-muted">
						{hooks[event.key].length} / {MAX_HOOKS_PER_EVENT} 条{hooks[event.key]
							.length === 0
							? ' · 尚未配置'
							: ''}
					</p>
					<div class="hook-command-list motion-list">
						{#each hooks[event.key] as hook, index (`${event.key}-${index}`)}
							<div class="hook-command-motion">
								<HookCommandEditor
									{hook}
									{index}
									count={hooks[event.key].length}
									eventLabel={event.label}
									veto={event.veto}
					toolEvent={event.key === 'pre_tool_use' ||
						event.key === 'permission_request' ||
						event.key === 'post_tool_use' ||
						event.key === 'toolrun_start' ||
						event.key === 'toolrun_end'}
									disabled={saving}
									onPatch={(patch) => patchHook(event.key, index, patch)}
									onMove={(offset) => moveHook(event.key, index, offset)}
									onRemove={() => removeHook(event.key, index)}
									onDuplicate={() => {
										if (hooks[event.key].length < MAX_HOOKS_PER_EVENT)
											replaceHookList(event.key, [
												...hooks[event.key].slice(0, index + 1),
												structuredClone($state.snapshot(hook)),
												...hooks[event.key].slice(index + 1),
											]);
									}}
								/>
							</div>
						{/each}
					</div>
				</section>
			{/each}
			{#if validationErrors.length}<p class="hooks-error motion-surface-enter" role="status">
					有 {validationErrors.length} 项待修正（包含筛选隐藏的事件），请查看各命令下方提示。
				</p>{/if}
			<HookConfigTransfer
				{hooks}
				disabled={saving}
				onImport={(draft: LifecycleHooksConfig) => {
					hooks = draft;
					errorMessage = '';
					eventFilter = 'all';
				}}
			/>

			<div class="hooks-save-row">
				<p class="hooks-note">
					Hook 通过 stdin 接收 JSON。会话输入和工具结果可能含敏感内容，请只配置可信命令。
				</p>
				<MaterialButton
					variant="filled"
					label={saving ? '保存中…' : '保存 Hooks'}
					disabled={saving || !hooksDirty || validationErrors.length > 0}
					onclick={() => void saveDraft()}
				/>
			</div>
			<MaterialButton
				variant="text"
				label="放弃改动"
				disabled={saving || !hooksDirty}
				onclick={discardDraft}
			/>
		{/if}
	</SettingsSection>

	<SettingsSection
		title="项目 Hook 信任"
		description="信任目录后，目录中的 .haven/hooks.toml 才会加入配置。用户级 Hooks 先执行，项目级 Hooks 后执行。"
	>
		<div class="trust-actions">
			<p class="hooks-note">
				项目 Hook 文件可由仓库内容修改，因此仅对你明确选择的本地目录启用。
			</p>
			<MaterialButton variant="outlined" label="选择并信任目录" onclick={trustWorkspace} />
		</div>
		{#if trustedWorkspaces.length === 0}
			<p class="hooks-muted motion-surface-enter">没有已信任的项目目录。</p>
		{:else}
			<ul class="trusted-workspaces motion-list">
				{#each trustedWorkspaces as path (path)}
					<li out:fade={{ duration: 140 }}>
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
			<MaterialButton
				variant="text"
				label="刷新"
				onclick={() => void refreshMetadata()}
				disabled={loading || refreshing}
			/>
		</div>
		{#if diagnostics.length === 0}
			<p class="hooks-muted motion-surface-enter">触发 Hook 后会在这里显示运行状态。</p>
		{:else}
			<div class="hook-diagnostics motion-list">
				{#each diagnostics as item (item.event)}
					<div class="hook-diagnostic-row">
						<strong>{item.event}</strong>
						<span>{outcomeLabel(item)}</span>
						<span
							>事件 {item.event_runs} 次 · 命令尝试 {item.command_attempts} 次 · 已启动
							{item.command_starts} 次</span
						>
						{#if item.failures || item.denials}
							<small
								>失败 {item.failures} · 脚本拒绝 {item.denials} · {item.last_failure_code ??
									''}{#if item.last_problem_command} · 最近问题命令：{item.last_problem_command.source ===
										'user'
										? '用户级'
										: '项目级'}第 {item.last_problem_command.index + 1} 条{/if}</small
							>
						{/if}
						{#if item.last_failure_code}<small
								>{hookFailureHelp(item.last_failure_code)}</small
							>{/if}
					</div>
				{/each}
			</div>
		{/if}
	</SettingsSection>
</div>

<style>
	.hooks-toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: end;
		gap: 0.75rem;
		margin-bottom: 0.8rem;
	}
	.hooks-toolbar label {
		display: grid;
		gap: 0.35rem;
		font-size: 0.875rem;
	}
	.hooks-toolbar span {
		font-size: 0.8125rem;
		color: var(--md-sys-color-on-surface-variant);
	}
	.settings-hooks {
		display: grid;
		gap: 1rem;
	}
	.hook-policy,
	.hook-event {
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
	.trust-actions,
	.diagnostics-toolbar,
	.trusted-workspaces li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.hook-event__header h3 {
		margin: 0;
		font-size: 1rem;
	}
	.hook-event__header p {
		margin: 0.3rem 0 0;
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
