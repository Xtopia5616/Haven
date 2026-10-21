<script lang="ts">
	import { untrack } from 'svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { inputElementValue, withStringValue } from '$lib/typedCallbacks.ts';
	import {
		describePermissionRuleImpact,
		permissionTargetLabel,
	} from '$lib/permissionRuleDescription.ts';
	import type { SecurityConfigInput } from '$lib/contracts/generatedCommands.ts';
	import type { SessionPermissionGrant } from '$lib/contracts/settings.ts';
	import { describeShellExecutionStatus } from '$lib/shellExecutionPresentation.ts';
	import type { ShellExecutionSecurityStatus } from '$lib/contracts/shellExecution.ts';

	interface Props {
		security: SecurityConfigInput & { permissions: Array<{ key: string; effect: string }> };
		sessionPermissions?: SessionPermissionGrant[];
		securityDirty?: boolean;
		securityRuntimeStatus?: 'current' | 'unchanged' | 'incomplete';
		securityRuntimeNotice?: string;
		shellExecutionStatus?: ShellExecutionSecurityStatus | null;
		shellExecutionStatusLoading?: boolean;
		shellExecutionStatusError?: boolean;
		onRefreshShellExecutionStatus?: () => void | Promise<void>;
		onRevokePermission?: (key: string) => boolean | Promise<boolean>;
		onRevokeSessionPermission?: (grant: SessionPermissionGrant) => boolean | Promise<boolean>;
		onResetPermissions?: () => boolean | Promise<boolean>;
		onResetSessionPermissions?: () => boolean | Promise<boolean>;
		onPickExecutionRoot?: () => Promise<string | null>;
	}

	/**
	 * Permission settings presentation. The route owns persistence and command
	 * calls; this component owns the user-facing policy choices and confirmation
	 * UI for immediate permission-rule changes.
	 */
	let {
		security,
		sessionPermissions = [],
		securityDirty = false,
		securityRuntimeStatus = 'current',
		securityRuntimeNotice = '安全策略已按当前配置完成运行时应用。',
		shellExecutionStatus = null,
		shellExecutionStatusLoading = false,
		shellExecutionStatusError = false,
		onRefreshShellExecutionStatus = () => {},
		onRevokePermission = async () => true,
		onRevokeSessionPermission = async () => true,
		onResetPermissions = async () => true,
		onResetSessionPermissions = async () => true,
		onPickExecutionRoot = async () => null,
	}: Props = $props();
	let pickingExecutionRoot = $state(false);
	let activeShellExecutionSummary = $derived(
		describeShellExecutionStatus(shellExecutionStatus),
	);

	async function pickExecutionRoot() {
		if (pickingExecutionRoot) return;
		pickingExecutionRoot = true;
		try {
			const selected = await onPickExecutionRoot();
			if (selected !== null) security.execution_root = selected;
		} finally {
			pickingExecutionRoot = false;
		}
	}

	const PERMISSION_MODES = [
		{
			value: 'default',
			label: '默认',
			shortLabel: '日常使用',
			detail: '普通只读操作自动完成；敏感读取、出网和编辑等操作按边界询问。',
		},
		{
			value: 'plan',
			label: '计划',
			shortLabel: '只分析不修改',
			detail: '仅允许只读分析，任何修改或副作用都会被拦截。',
		},
		{
			value: 'auto_edit',
			label: '自动编辑',
			shortLabel: '放心编辑工作区',
			detail: '明确的工作区文件编辑可自动接受；命令、网络、界面和高风险操作仍需确认。',
		},
		{
			value: 'autonomous',
			label: '自动',
			shortLabel: '少打断',
			detail: '非关键操作自动执行，高风险与关键操作仍需确认。',
		},
	] as const;

	const SANDBOX_MODES = [
		{
			value: 'read_only',
			label: '只读模式',
			detail: '拒绝本地文件写入；不会仅因只读模式而一概拒绝网络请求、打开外链等外部效果，它们仍按网络策略、操作风险与确认规则处理。无法保证只读的工具或子进程仍可能按执行边界规则拒绝。',
		},
		{
			value: 'workspace_write',
			label: '工作区可写',
			detail: '本地文件写操作受工作区范围约束；不会仅因目标不在可写根内而拒绝其它外部效果，它们仍按网络策略、操作风险与确认规则处理。无法由执行边界约束文件访问的子进程仍会被拒绝。',
		},
		{
			value: 'full_access',
			label: '完整用户权限',
			detail: '允许时，子进程以当前 Windows 用户权限访问该用户可访问的文件；执行工作区只决定默认工作目录，联网仍按单独的网络策略处理。Haven 将每棵受管进程树限制为最多 256 个活动进程。',
		},
	] as const;

	const NETWORK_POLICIES = [
		{
			value: 'deny',
			label: '禁止网络',
			detail: 'Haven 拒绝声明需要网络的工具；这不是 Windows 防火墙规则，不会限制其它程序或已经运行的进程。',
		},
		{
			value: 'ask',
			label: '请求确认',
			detail: '可验证的公网请求先询问；工作区可写模式会拦截无法验证目的地的 MCP、技能和命令子进程。完整访问模式下经确认启动的子进程仍按当前 Windows 用户权限联网。',
		},
		{
			value: 'restricted',
			label: '受限网络',
			detail: '只允许 Haven 能验证目的地的公共网络请求；无法按目的地检查的 MCP、技能和命令子进程会被拒绝。这不会限制其它程序或已运行进程的 OS 网络权限。',
		},
		{
			value: 'open',
			label: '开放网络',
			detail: 'Haven 不添加全局网络封锁或确认，工具自身授权仍会执行；获准启动的子进程按当前 Windows 用户权限联网。',
		},
	] as const;

	/** @type {Record<string, string>} */
	const RULE_LABELS: Record<string, string> = {
		shell: '执行本机命令',
		files: '文件操作',
		http: '网络请求',
		system: '系统能力',
		window: '窗口控制',
		input: '模拟输入',
		memory: '长期记忆',
		clipboard: '剪贴板',
		tool_runs: '任务管理',
		agent: '代理协作',
		media: '媒体处理',
		schedule: '定时任务',
	};

	/** @type {Record<string, string>} */
	const OPERATION_LABELS: Record<string, string> = {
		read: '读取',
		list: '查看',
		search: '搜索',
		write: '写入',
		edit: '编辑',
		patch: '修改',
		create: '创建',
		create_dir: '创建目录',
		copy: '复制',
		move: '移动',
		delete: '删除',
		run: '执行',
		kill: '结束进程',
		set: '修改设置',
		power: '电源',
		env: '环境变量',
		registry: '注册表',
		lock: '锁定屏幕',
		status: '查看状态',
		open: '打开',
		speak: '朗读',
		inspect: '检查',
		spawn: '创建代理',
		cancel: '取消',
	};

	let resetDialogOpen = $state(false);
	let resetPending = $state(false);
	let pendingRule = $state('');
	let advancedBoundaryOpen = $state(untrack(() => (security.writable_roots?.length ?? 0) > 0));
	let resetScope = $state<'permanent' | 'session'>('permanent');
	let permissions = $derived(Array.isArray(security?.permissions) ? security.permissions : []);
	let storedSessionPermissions = $derived(
		Array.isArray(sessionPermissions) ? sessionPermissions : [],
	);
	let writableRoots = $derived(
		Array.isArray(security?.writable_roots) ? security.writable_roots.filter(Boolean) : [],
	);
	let securitySummaryStatus = $derived(
		securityDirty
			? '未保存'
			: securityRuntimeStatus === 'unchanged'
				? '仍按旧策略运行'
				: securityRuntimeStatus === 'incomplete'
					? '安全更新未完成'
					: '已应用',
	);
	let selectedPermissionMode = $derived(
		PERMISSION_MODES.find((mode) => mode.value === security?.permission_mode) ||
			PERMISSION_MODES[0],
	);
	let selectedSandboxMode = $derived(
		SANDBOX_MODES.find((mode) => mode.value === security?.sandbox_mode) || SANDBOX_MODES[1],
	);
	let selectedNetworkPolicy = $derived(
		NETWORK_POLICIES.find((policy) => policy.value === security?.network_policy) ||
			NETWORK_POLICIES[1],
	);
	function isOptionValue<T extends string>(
		options: readonly { value: T }[],
		value: string,
	): value is T {
		return options.some((option) => option.value === value);
	}

	function permissionRuleMeta(key: string) {
		const parts = String(key || '')
			.split('.')
			.filter(Boolean);
		const rootLabel = RULE_LABELS[parts[0]] || parts[0] || '未知能力';
		if (parts.length <= 1) {
			return { label: rootLabel };
		}

		const operation = parts.at(-1) || '';
		const operationLabel = OPERATION_LABELS[operation] || operation;
		if (parts.length === 2) {
			return { label: `${rootLabel} · ${operationLabel}` };
		}

		return { label: `${rootLabel} · ${operationLabel}` };
	}

	function openResetDialog(scope: 'permanent' | 'session') {
		if (!resetPending) {
			resetScope = scope;
			resetDialogOpen = true;
		}
	}

	function closeResetDialog() {
		if (!resetPending) resetDialogOpen = false;
	}

	async function revokePermission(key: string) {
		pendingRule = key;
		try {
			await onRevokePermission?.(key);
		} finally {
			if (pendingRule === key) pendingRule = '';
		}
	}

	async function confirmResetPermissions() {
		resetPending = true;
		try {
			const result = await (resetScope === 'permanent'
				? onResetPermissions?.()
				: onResetSessionPermissions?.());
			if (result !== false) resetDialogOpen = false;
		} finally {
			resetPending = false;
		}
	}
</script>

<SettingsSection
	title="权限中心"
	description="管理默认安全边界、跨会话永久规则和仅对指定会话生效的授权。"
>
	<div class="security-summary">
		<div>
			<span class="summary-eyebrow">当前默认策略</span>
			<strong>{selectedPermissionMode.label} · {selectedPermissionMode.shortLabel}</strong>
			<p>{selectedPermissionMode.detail}</p>
		</div>
		<div class="summary-runtime-wrap">
			<span
				class="summary-status"
				class:summary-status--warning={!securityDirty &&
					securityRuntimeStatus !== 'current'}>{securitySummaryStatus}</span
			>
			<p
				class="summary-runtime"
				class:summary-runtime--warning={!securityDirty &&
					securityRuntimeStatus !== 'current'}
			>
				{securityDirty
					? '这些安全设置尚未保存；当前进程继续使用已经加载的策略。'
					: securityRuntimeNotice}
			</p>
		</div>
	</div>

	<div class="active-shell-status" aria-label="当前 Shell 执行状态" aria-live="polite">
		<div class="active-shell-status__heading">
			<div>
				<span class="summary-eyebrow">进程当前实际使用的执行边界</span>
				<strong>
					{shellExecutionStatusLoading
						? '正在读取…'
						: activeShellExecutionSummary.title}
				</strong>
			</div>
			<MaterialButton
				variant="text"
				label={shellExecutionStatusLoading ? '正在读取' : '刷新状态'}
				disabled={shellExecutionStatusLoading}
				onclick={onRefreshShellExecutionStatus}
			/>
		</div>
		<p>{activeShellExecutionSummary.details}</p>
		<div class="active-shell-status__network">
			<strong>{activeShellExecutionSummary.networkTitle}</strong>
			<p>{activeShellExecutionSummary.networkDetails}</p>
		</div>
		{#if securityDirty}
			<p class="active-shell-status__note">上方是当前进程正在使用的策略；尚未保存的选项不会改变这条状态。</p>
		{/if}
		{#if shellExecutionStatusError}
			<p class="active-shell-status__note active-shell-status__note--warning">
				读取失败。无法确认隔离状态；Haven 会在执行前再次检查授权和隔离门槛。
			</p>
		{:else if shellExecutionStatus?.sandboxMode === 'workspace_write' &&
			shellExecutionStatus.brokerGateStatus !== 'attested'}
			<p class="active-shell-status__note active-shell-status__note--warning">
				当前 broker 配额 gate 未通过；Workspace Write Shell 会被拒绝，不会回退到本机 Shell。
			</p>
		{/if}
	</div>

	<div class="settings-subsection">
		<div class="subsection-heading">
			<div>
				<h3>默认行为</h3>
				<p>只决定 Haven 遇到需要授权的操作时的默认处理方式，关键安全底线不会被关闭。</p>
			</div>
		</div>
		<div class="policy-grid" role="radiogroup" aria-label="默认权限策略">
			{#each PERMISSION_MODES as mode (mode.value)}
				<label
					class="policy-option"
					class:selected={security.permission_mode === mode.value}
				>
					<input
						type="radio"
						name="security-mode"
						value={mode.value}
						checked={security.permission_mode === mode.value}
						onchange={() => {
							security.permission_mode = mode.value;
						}}
					/>
					<span class="policy-option__body">
						<strong>{mode.label}</strong>
						<span>{mode.shortLabel}</span>
					</span>
				</label>
			{/each}
		</div>
	</div>

	<div class="settings-subsection">
		<div class="subsection-heading">
			<div>
				<h3>安全边界</h3>
				<p>
					这些策略由 Haven 授权网关执行；不会替其它进程设置 Windows
					文件或网络限制，受管子进程还会遵守各自的执行边界。
				</p>
			</div>
		</div>
		<div class="boundary-grid">
			<div class="boundary-card">
				<SettingsField label="文件访问模式" id="security-sandbox">
					<MaterialSelect
						id="security-sandbox"
						value={security.sandbox_mode || 'workspace_write'}
						options={SANDBOX_MODES.map((mode) => ({
							value: mode.value,
							label: mode.label,
						}))}
						onChange={withStringValue((value) => {
							if (isOptionValue(SANDBOX_MODES, value)) security.sandbox_mode = value;
						})}
					/>
				</SettingsField>
				<p class="boundary-detail">{selectedSandboxMode.detail}</p>
			</div>
			<div class="boundary-card">
				<SettingsField label="网络策略" id="security-network">
					<MaterialSelect
						id="security-network"
						value={security.network_policy || 'ask'}
						options={NETWORK_POLICIES.map((policy) => ({
							value: policy.value,
							label: policy.label,
						}))}
						onChange={withStringValue((value) => {
							if (isOptionValue(NETWORK_POLICIES, value))
								security.network_policy = value;
						})}
					/>
				</SettingsField>
				<p class="boundary-detail">{selectedNetworkPolicy.detail}</p>
			</div>
		</div>

		<div class="execution-workspace-boundary">
			<div class="subsection-heading">
				<div>
					<h3>执行工作区</h3>
					<p>
						Files 的相对路径和默认 Shell 工作目录使用此根。工作区可写模式下，只有
						writable_roots
						留空时它才是默认写入范围；自定义可写目录非空时会覆盖此默认范围。Files
						工具配置的 allowed_paths 还会进一步收窄写入范围。
						已开始的调用继续使用旧工作区；子进程是否隔离由各自的执行边界决定。
					</p>
				</div>
			</div>
			<SettingsField
				label="执行工作区目录"
				id="security-execution-root"
				description="填写已存在的本地绝对路径；应用设置后，新调用使用新目录。"
				stacked
			>
				<div class="execution-workspace-input">
					<textarea
						id="security-execution-root"
						class="md-textarea md-textarea--code security-roots"
						rows="1"
						value={security.execution_root || ''}
						placeholder="留空使用启动时发现的仓库根"
						oninput={(event) => {
							security.execution_root = inputElementValue(event).trim() || null;
						}}></textarea>
					<MaterialButton
						variant="outlined"
						disabled={pickingExecutionRoot}
						ariaBusy={pickingExecutionRoot}
						onclick={pickExecutionRoot}
					>
						{pickingExecutionRoot ? '正在选择…' : '选择目录'}
					</MaterialButton>
				</div>
			</SettingsField>
		</div>

		<div class="advanced-boundary">
			<MaterialCollapsible bind:open={advancedBoundaryOpen}>
				{#snippet header()}
					<span>高级：自定义可写目录</span>
					<small>{security.writable_roots?.length || 0} 个目录</small>
				{/snippet}
				<div class="advanced-boundary__body">
					<p>
						每行填写一个已存在、可安全打开的本地绝对目录。非空时，这些目录覆盖执行工作区的默认写入范围；为空时使用当前执行工作区。Files
						工具配置的 allowed_paths 会与写入基准取交集。
					</p>
					<textarea
						id="security-writable-roots"
						class="md-textarea md-textarea--code security-roots"
						rows="3"
						value={(security.writable_roots || []).join('\n')}
						placeholder="例如：C:\\Users\\me\\Projects\\haven"
						oninput={(event) => {
							security.writable_roots = inputElementValue(event)
								.split(/\r?\n/)
								.map((value) => value.trim())
								.filter(Boolean);
						}}></textarea>
					<div class="effective-write-scope" aria-live="polite">
						<strong>工作区可写模式的写入基准</strong>
						{#if security.sandbox_mode === 'read_only'}
							<p>
								当前只读模式会拒绝文件修改；切换为工作区可写模式后，以下基准范围生效。
							</p>
						{:else if security.sandbox_mode === 'full_access'}
							<p>
								当前完整用户权限模式不使用工作区写入边界；Files 工具的 allowed_paths
								仍按各自配置生效。
							</p>
						{:else}
							<p>当前工作区写入范围还会与 Files 工具的 allowed_paths 取交集。</p>
						{/if}
						{#if writableRoots.length > 0}
							<ul aria-label="当前自定义写入基准">
								{#each writableRoots as root (root)}
									<li><code>{root}</code></li>
								{/each}
							</ul>
						{:else}
							<p class="effective-write-scope__root">
								{security.execution_root || '启动时发现的执行工作区'}
							</p>
						{/if}
					</div>
				</div>
			</MaterialCollapsible>
		</div>
	</div>

	<div class="rules-section">
		<div class="subsection-heading rules-heading">
			<div>
				<h3>永久规则</h3>
				<p>这些规则来自确认弹窗中的“始终允许 / 始终拒绝”，会跨会话保留。</p>
			</div>
			{#if permissions.length > 0}<span class="rules-count">{permissions.length} 条</span
				>{/if}
		</div>

		{#if permissions.length > 0}
			<div class="perm-list" role="list" aria-label="已保存的权限规则">
				{#each permissions as perm (perm.key)}
					{@const meta = permissionRuleMeta(perm.key)}
					<div class="perm-row motion-list-item" role="listitem">
						<div
							class="perm-indicator"
							class:deny={perm.effect === 'deny'}
							aria-hidden="true"
						>
							{perm.effect === 'deny' ? '!' : '✓'}
						</div>
						<div class="perm-copy">
							<strong>{meta.label}</strong>
							<span class="perm-impact">
								{describePermissionRuleImpact(perm.key, perm.effect)}
							</span>
							<code class="perm-key">{perm.key}</code>
						</div>
						<span class="perm-effect" class:deny={perm.effect === 'deny'}>
							{perm.effect === 'deny' ? '始终拒绝' : '始终允许'}
						</span>
						<MaterialButton
							variant="danger-text"
							className="perm-revoke"
							label={pendingRule === perm.key ? '撤销中…' : '撤销'}
							disabled={pendingRule !== ''}
							ariaBusy={pendingRule === perm.key}
							onclick={() => revokePermission(perm.key)}
						/>
					</div>
				{/each}
			</div>
			<MaterialButton
				variant="danger-text"
				className="permission-reset"
				label={`清除 ${permissions.length} 条永久规则`}
				onclick={() => openResetDialog('permanent')}
				disabled={pendingRule !== '' || resetPending}
			/>
		{:else}
			<div class="empty-rules">
				<strong>还没有永久规则</strong>
				<p>需要长期记住某个允许或拒绝决定时，可以在权限确认弹窗中选择对应选项。</p>
			</div>
		{/if}
	</div>

	<div class="rules-section">
		<div class="subsection-heading rules-heading">
			<div>
				<h3>会话授权</h3>
				<p>
					只对列出的会话生效。允许和拒绝决定都保存在本机，不会按时间自动到期；会话重新打开时仍会恢复。逐条撤销、整组清除或删除会话（含保留期清理）后移除。
				</p>
			</div>
			{#if storedSessionPermissions.length > 0}
				<span class="rules-count">{storedSessionPermissions.length} 条</span>
			{/if}
		</div>
		{#if storedSessionPermissions.length > 0}
			<div class="perm-list" role="list" aria-label="已保存的会话授权">
				{#each storedSessionPermissions as grant (`${grant.sessionId}:${grant.capability}`)}
					{@const meta = permissionRuleMeta(grant.capability)}
					{@const grantId = `${grant.sessionId}:${grant.capability}`}
					<div class="perm-row motion-list-item" role="listitem">
						<div
							class="perm-indicator"
							class:deny={grant.effect === 'deny'}
							aria-hidden="true"
						>
							{grant.effect === 'deny' ? '!' : '✓'}
						</div>
						<div class="perm-copy">
							<strong>{meta.label}</strong>
							<span class="perm-scope"
								>目标层级：{permissionTargetLabel(grant.target)}</span
							>
							<span class="perm-impact">
								{describePermissionRuleImpact(grant.capability, grant.effect)}
							</span>
							<span class="perm-scope">{grant.session_title || grant.sessionId}</span>
							<code class="perm-key">{grant.sessionId} · {grant.capability}</code>
						</div>
						<span class="perm-effect" class:deny={grant.effect === 'deny'}>
							{grant.effect === 'deny' ? '本会话拒绝' : '本会话允许'}
						</span>
						<MaterialButton
							variant="danger-text"
							className="perm-revoke"
							label={pendingRule === grantId ? '撤销中…' : '撤销'}
							disabled={pendingRule !== ''}
							ariaBusy={pendingRule === grantId}
							onclick={async () => {
								pendingRule = grantId;
								try {
									await onRevokeSessionPermission?.(grant);
								} finally {
									if (pendingRule === grantId) pendingRule = '';
								}
							}}
						/>
					</div>
				{/each}
			</div>
			<MaterialButton
				variant="danger-text"
				className="permission-reset"
				label={`清除 ${storedSessionPermissions.length} 条会话授权`}
				onclick={() => openResetDialog('session')}
				disabled={pendingRule !== '' || resetPending}
			/>
		{:else}
			<div class="empty-rules">
				<strong>还没有会话授权</strong>
				<p>在会话确认中选择“本会话允许”或“本会话拒绝”后，授权会显示在这里。</p>
			</div>
		{/if}
	</div>
</SettingsSection>

<MaterialDialog
	open={resetDialogOpen}
	title={resetScope === 'permanent' ? '清除永久规则' : '清除会话授权'}
	onClose={closeResetDialog}
>
	{#snippet children()}
		<p class="reset-dialog-copy">
			{#if resetScope === 'permanent'}
				这会清除 {permissions.length} 条“始终允许”和“始终拒绝”规则，保留所有会话授权和默认安全策略。
			{:else}
				这会清除 {storedSessionPermissions.length} 条会话允许与拒绝决定，保留永久规则和默认安全策略。
			{/if}
		</p>
	{/snippet}
	{#snippet footer()}
		<MaterialButton
			variant="text"
			label="取消"
			onclick={closeResetDialog}
			disabled={resetPending}
		/>
		<MaterialButton
			variant="danger"
			label={resetPending ? '清除中…' : '确认清除'}
			onclick={confirmResetPermissions}
			disabled={resetPending}
			ariaBusy={resetPending}
		/>
	{/snippet}
</MaterialDialog>

<style>
	.active-shell-status {
		margin-top: var(--md-sys-space-lg);
		padding: var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: var(--md-sys-color-surface-container-low);
	}

	.active-shell-status__heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}

	.active-shell-status__heading > div {
		display: grid;
		gap: var(--md-sys-space-xs);
	}

	.active-shell-status__heading strong,
	.active-shell-status__network strong {
		color: var(--md-sys-color-on-surface);
	}

	.active-shell-status p {
		margin: var(--md-sys-space-sm) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: 1.5;
	}

	.active-shell-status__network {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-sm);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}

	.active-shell-status__note {
		font-weight: 600;
	}

	.active-shell-status__note--warning {
		color: var(--md-sys-color-error) !important;
	}

	.security-summary {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-xl);
		padding: var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-primary);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.summary-eyebrow {
		display: block;
		margin-bottom: var(--md-sys-space-2xs);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}
	.security-summary strong {
		display: block;
		font-size: var(--md-sys-typescale-title-large-size);
		line-height: var(--md-sys-typescale-title-large-line-height);
	}
	.security-summary p {
		max-width: 680px;
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-primary-container);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.summary-status,
	.rules-count {
		flex-shrink: 0;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-lowest);
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		white-space: nowrap;
	}
	.summary-runtime-wrap {
		flex: 0 1 340px;
		text-align: right;
	}
	.summary-status--warning {
		color: var(--md-sys-color-error);
	}
	.security-summary .summary-runtime {
		margin-top: var(--md-sys-space-xs);
	}
	.security-summary .summary-runtime--warning {
		color: var(--md-sys-color-error);
	}
	.settings-subsection,
	.rules-section {
		margin-top: var(--md-sys-space-xl);
	}
	.subsection-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-md);
	}
	.subsection-heading h3 {
		margin: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-medium-size);
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}
	.subsection-heading p,
	.boundary-detail,
	.advanced-boundary__body p,
	.effective-write-scope p,
	.empty-rules p,
	.reset-dialog-copy {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.policy-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 180px), 1fr));
		gap: var(--md-sys-space-sm);
	}
	.policy-option {
		position: relative;
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
		min-height: 72px;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		cursor: pointer;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.policy-option:hover {
		border-color: var(--md-sys-color-primary);
	}
	.policy-option.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary-container);
	}
	.policy-option input {
		accent-color: var(--md-sys-color-primary);
		margin: var(--md-sys-space-2xs) 0 0;
	}
	.policy-option__body {
		display: grid;
		gap: var(--md-sys-space-2xs);
	}
	.policy-option__body strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-medium-size);
	}
	.policy-option__body span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.boundary-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
		gap: var(--md-sys-space-md);
	}
	.boundary-card {
		padding: var(--md-sys-space-sm) var(--md-sys-space-md) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.boundary-card .boundary-detail {
		margin-top: var(--md-sys-space-sm);
	}
	.advanced-boundary {
		margin-top: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	.advanced-boundary :global(.md-collapsible-header) {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 700;
	}
	.advanced-boundary :global(.md-collapsible-header-content) {
		justify-content: space-between;
	}
	.advanced-boundary :global(.md-collapsible-header-content small) {
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 400;
	}
	.advanced-boundary__body {
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-md);
	}
	.security-roots {
		margin-top: var(--md-sys-space-sm);
	}
	.effective-write-scope {
		display: grid;
		gap: var(--md-sys-space-2xs);
		margin-top: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container);
	}
	.effective-write-scope strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.effective-write-scope ul {
		display: grid;
		gap: var(--md-sys-space-2xs);
		margin: var(--md-sys-space-2xs) 0 0;
		padding-left: var(--md-sys-space-lg);
	}
	.effective-write-scope li,
	.effective-write-scope code {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-small-size);
		overflow-wrap: anywhere;
	}
	.effective-write-scope__root {
		font-family: var(--md-sys-typescale-body-small-font);
	}
	.execution-workspace-input {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
	}
	.execution-workspace-input textarea {
		flex: 1 1 auto;
		min-width: 0;
	}
	.rules-section {
		padding-top: var(--md-sys-space-xl);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.rules-heading {
		align-items: center;
	}
	.perm-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.perm-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-lowest);
	}
	.perm-indicator {
		display: grid;
		place-items: center;
		width: var(--md-sys-space-xl);
		height: var(--md-sys-space-xl);
		flex: 0 0 auto;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		font-weight: 700;
	}
	.perm-indicator.deny {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.perm-copy {
		display: grid;
		gap: var(--md-sys-space-2xs);
		min-width: 0;
		flex: 1;
	}
	.perm-copy strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
	}
	.perm-scope {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
	}
	.perm-impact {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.perm-key {
		max-width: 100%;
		overflow: hidden;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.perm-effect {
		flex-shrink: 0;
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		white-space: nowrap;
	}
	.perm-effect.deny {
		color: var(--md-sys-color-error);
	}
	:global(.md-btn.perm-revoke) {
		flex-shrink: 0;
	}
	:global(.md-btn.permission-reset) {
		margin-top: var(--md-sys-space-sm);
	}
	.empty-rules {
		padding: var(--md-sys-space-lg);
		border: 1px dashed var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	.empty-rules strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
	}

	@container settings-content (max-width: 640px) {
		.security-summary,
		.perm-row {
			align-items: stretch;
			flex-direction: column;
		}
		.summary-runtime-wrap {
			text-align: left;
		}
		.summary-status,
		.rules-count {
			align-self: flex-start;
		}
		.perm-effect,
		:global(.md-btn.perm-revoke) {
			align-self: flex-start;
		}
	}
</style>
