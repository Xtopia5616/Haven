<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { inputElementValue, withStringValue } from '$lib/typedCallbacks.ts';
	import {
		describePermissionRuleImpact,
		permissionTargetLabel,
	} from '$lib/permissionRuleDescription.ts';
	import { permissionResourceScopeDescription } from '$lib/permissionResourceDescription.ts';
	import type {
		FileWriteModeInput,
		MxcIsolationModeInput,
		NetworkAccessBoundaryInput,
	} from '$lib/contracts/generatedCommands.ts';
import type { SecuritySettingsDraft, SessionPermissionGrant } from '$lib/contracts/settings.ts';

	interface Props {
		security: SecuritySettingsDraft;
		sessionPermissions?: SessionPermissionGrant[];
		securityDirty?: boolean;
		securityRuntimeStatus?: 'current' | 'unchanged' | 'incomplete';
		securityRuntimeNotice?: string;
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
		onRevokePermission = async () => true,
		onRevokeSessionPermission = async () => true,
		onResetPermissions = async () => true,
		onResetSessionPermissions = async () => true,
		onPickExecutionRoot = async () => null,
	}: Props = $props();
	let pickingExecutionRoot = $state(false);

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

	const EXECUTION_MODES = [
		{
			value: 'analyze',
			label: '只分析',
			shortLabel: '不修改',
			detail: '只允许只读分析；需要确认的敏感读取仍会询问。',
		},
		{
			value: 'collaborative',
			label: '协作执行',
			shortLabel: '日常工作',
			detail: '在授权范围内执行普通操作；需要额外批准时会询问。',
		},
		{
			value: 'unattended',
			label: '无人值守',
			shortLabel: '需要确认时停止',
			detail: '只执行无需追加确认且已获准的操作；遇到需要确认的操作会停止。',
		},
	] as const;

	const MXC_ISOLATION_MODES: Array<{
		value: MxcIsolationModeInput;
		label: string;
		detail: string;
	}> = [
		{
			value: 'strict',
			label: '严格 MXC',
			detail:
				'只使用不需 DACL 补强的 MXC BaseContainer。探测或启动失败时拒绝执行，不自动切换隔离层或收窄网络。',
		},
		{
			value: 'compatibility',
			label: '兼容隔离',
			detail:
				'BaseContainer 无法启动时允许 MXC AppContainer 兼容层，并可能将网络收窄为仅出站或完全断网。DACL 层会临时调整授权目录 ACL，并在子进程结束时恢复。',
		},
	];

	const FILE_WRITE_MODES = [
		{
			value: 'read_only',
			label: '只读文件',
			detail: '拒绝本地文件写入。网络和其它外部效果继续按同一资源策略中的对应规则处理。',
		},
		{
			value: 'workspace',
			label: '工作区范围',
			detail: '写入限于当前会话工作目录，或下方配置的明确写入目录。工具自己的路径规则还会进一步收窄范围。',
		},
		{
			value: 'roots',
			label: '指定目录',
			detail: '写入只允许在下方列出的本地目录中进行；目录必须存在且可安全打开。',
		},
		{
			value: 'unrestricted',
			label: '不附加路径限制',
			detail: '移除 Haven 管理的路径范围限制；操作仍受授权、风险规则和当前 Windows 用户权限约束。',
		},
	] as const;

	const NETWORK_POLICIES = [
		{
			value: 'deny',
			label: '禁止网络',
			detail: 'Haven 拒绝声明需要网络的工具；这不是 Windows 防火墙规则，不会限制其它程序或已经运行的进程。',
		},
		{
			value: 'restricted',
			label: '受限网络',
			detail: '只允许 Haven 能验证目的地的公共网络请求；无法按目的地检查的 MCP、技能和命令子进程会被拒绝。是否询问由执行方式决定。',
		},
		{
			value: 'open',
			label: '开放网络',
			detail: 'Haven 不添加目的地限制；工具自身校验仍会执行，操作是否需要确认由执行方式决定。',
		},
	] as const;

	/** @type {Record<string, string>} */
	const RULE_LABELS: Record<string, string> = {
		haven: 'Haven',
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
		session: '会话',
		lsp: '代码能力',
		media: '媒体处理',
		schedule: '定时任务',
	};

	/** @type {Record<string, string>} */
	const OPERATION_LABELS: Record<string, string> = {
		read: '读取',
		get: '读取',
		list: '查看',
		list_children: '查看子代理',
		list_document_symbols: '查看文档符号',
		list_displays: '查看显示器',
		list_errors: '查看错误',
		list_history: '查看历史',
		list_inbox: '查看收件箱',
		list_links: '查看会话链接',
		list_sessions: '查看会话',
		search: '搜索',
		search_assets: '搜索媒体资产',
		search_history: '搜索历史',
		search_linked_history: '搜索关联历史',
		search_workspace_symbols: '搜索工作区符号',
		summarize: '生成摘要',
		write: '写入',
		edit: '编辑',
		patch: '修改',
		create: '创建',
		create_directory: '创建目录',
		add: '添加',
		acknowledge: '确认消息',
		capture_screenshot: '截图',
		connect: '连接',
		copy: '复制',
		disconnect: '断开连接',
		enable: '启用',
		disable: '停用',
		find_definition: '查找定义',
		find_implementations: '查找实现',
		find_references: '查找引用',
		find_type_definition: '查找类型定义',
		get_diagnostics: '查看诊断',
		get_foreground: '查看前台窗口',
		get_hover: '查看悬停信息',
		get_info: '查看系统信息',
		get_mute: '查看静音状态',
		get_outline: '查看大纲',
		get_profile: '查看资料',
		get_status: '查看状态',
		get_ui_tree: '查看界面结构',
		get_volume: '获取音量',
		move: '移动',
		read_history: '读取历史',
		read_linked_history: '读取关联历史',
		reload: '重新加载',
		remove: '移除',
		run_ocr: '识别文字',
		set_log_level: '设置日志等级',
		set_mute: '设置静音状态',
		set_volume: '设置音量',
		tail_logs: '查看最近日志',
		toggle: '切换状态',
		update: '更新',
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
	let resetScope = $state<'permanent' | 'session'>('permanent');
	let permissions = $derived(Array.isArray(security?.permissions) ? security.permissions : []);
	let storedSessionPermissions = $derived(
		Array.isArray(sessionPermissions) ? sessionPermissions : [],
	);
	let writeRoots = $derived(
		Array.isArray(security?.resource_policy?.files?.write_roots)
			? security.resource_policy.files.write_roots.filter(Boolean)
			: [],
	);
	let filePathRoots = $derived(
		Array.isArray(security?.resource_policy?.files?.path_roots)
			? security.resource_policy.files.path_roots.filter(Boolean)
			: [],
	);
	let readOnlyRoots = $derived(
		Array.isArray(security?.resource_policy?.files?.read_only_roots)
			? security.resource_policy.files.read_only_roots.filter(Boolean)
			: [],
	);
	let deniedPaths = $derived(
		Array.isArray(security?.resource_policy?.files?.denied_paths)
			? security.resource_policy.files.denied_paths.filter(Boolean)
			: [],
	);
	let networkAllowedDomains = $derived(
		Array.isArray(security?.resource_policy?.network?.allowed_domains)
			? security.resource_policy.network.allowed_domains.filter(Boolean)
			: [],
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
	let selectedExecutionMode = $derived(
		EXECUTION_MODES.find((mode) => mode.value === security?.execution_mode) ||
			EXECUTION_MODES[1],
	);
	let selectedFileWriteMode = $derived(
		FILE_WRITE_MODES.find((mode) => mode.value === security?.resource_policy?.files?.write_mode) ||
			FILE_WRITE_MODES[1],
	);
	let selectedNetworkPolicy = $derived(
		NETWORK_POLICIES.find((policy) => policy.value === security?.resource_policy?.network?.access) ||
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
	description="设置新会话的执行方式，并查看明确授权与资源边界。当前会话的执行方式在标题栏单独选择。"
>
	<div class="security-summary">
		<div>
			<span class="summary-eyebrow">当前默认策略</span>
			<strong>{selectedExecutionMode.label} · {selectedExecutionMode.shortLabel}</strong>
			<p>{selectedExecutionMode.detail}</p>
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

	<div class="settings-subsection">
		<div class="subsection-heading">
			<div>
				<h3>新会话默认执行方式</h3>
				<p>更改只影响之后创建的会话；每个会话可在标题栏单独选择。</p>
			</div>
		</div>
		<div class="policy-grid" role="radiogroup" aria-label="新会话默认执行方式">
			{#each EXECUTION_MODES as mode (mode.value)}
				<label
					class="policy-option"
					class:selected={security.execution_mode === mode.value}
				>
					<input
						type="radio"
						name="security-mode"
						value={mode.value}
						checked={security.execution_mode === mode.value}
						onchange={() => {
							security.execution_mode = mode.value;
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

	<div class="settings-subsection resource-boundaries">
		<div class="subsection-heading">
			<div>
				<h3>资源策略</h3>
				<p>
					文件和网络限制由同一策略描述，工具自己的资源限制只会进一步缩小范围。一次确认可临时扩展到弹窗列出的资源；它不会保存为新的策略。
				</p>
			</div>
		</div>
		<div class="boundary-grid">
			<div class="boundary-card">
				<SettingsField label="文件写入范围" id="security-file-write-mode">
					<MaterialSelect
						id="security-file-write-mode"
						value={security.resource_policy.files.write_mode || 'workspace'}
						options={FILE_WRITE_MODES.map((mode) => ({
							value: mode.value,
							label: mode.label,
						}))}
						onChange={withStringValue((value) => {
							if (isOptionValue(FILE_WRITE_MODES, value))
								security.resource_policy.files.write_mode = value as FileWriteModeInput;
						})}
					/>
				</SettingsField>
				<p class="boundary-detail">{selectedFileWriteMode.detail}</p>
			</div>
			<div class="boundary-card">
				<SettingsField label="网络策略" id="security-network">
					<MaterialSelect
						id="security-network"
						value={security.resource_policy.network.access || 'restricted'}
						options={NETWORK_POLICIES.map((policy) => ({
							value: policy.value,
							label: policy.label,
						}))}
						onChange={withStringValue((value) => {
							if (isOptionValue(NETWORK_POLICIES, value))
								security.resource_policy.network.access =
									value as NetworkAccessBoundaryInput;
						})}
					/>
				</SettingsField>
				<p class="boundary-detail">{selectedNetworkPolicy.detail}</p>
			</div>
		</div>

		<div class="execution-workspace-boundary">
			<div class="subsection-heading">
				<div>
					<h3>新会话默认工作目录</h3>
					<p>
						此目录只用于初始化新会话。每个会话随后独立保存工作目录，Files 的相对路径和
						Shell 默认目录会跟随该会话。工作区范围模式下，未配置写入目录时使用会话工作目录；指定目录模式会完全按目录列表约束写入。
						已开始的调用继续使用旧工作区；子进程是否隔离由各自的执行边界决定。
					</p>
				</div>
			</div>
			<SettingsField
				label="新会话默认目录"
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
						placeholder="留空使用启动时发现的项目根"
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

		<div class="resource-boundary-details" data-setting-searchable>
			<SettingsField
				label="写入目录"
				id="security-write-roots"
				description="每行一个已存在、可安全打开的本地绝对目录。工具级路径规则会与此范围取交集。"
				stacked
			>
				<textarea
					id="security-write-roots"
					class="md-textarea md-textarea--code security-roots"
					rows="3"
					value={writeRoots.join('\n')}
					placeholder="例如：C:\\Users\\me\\Projects\\haven"
					oninput={(event) => {
						const roots = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
						security.resource_policy.files.write_roots = roots;
						if (roots.length > 0 && security.resource_policy.files.write_mode === 'workspace') {
							security.resource_policy.files.write_mode = 'roots';
						} else if (
							roots.length === 0 && security.resource_policy.files.write_mode === 'roots'
						) {
							security.resource_policy.files.write_mode = 'workspace';
						}
					}}></textarea>
			</SettingsField>
			<SettingsField
				label="文件访问目录范围"
				id="security-file-path-roots"
				description="可选；每行一个已存在的本地绝对目录。它会同时限制声明的文件读取和写入范围，并与写入范围取交集。"
				stacked
			>
				<textarea
					id="security-file-path-roots"
					class="md-textarea md-textarea--code security-roots"
					rows="3"
					value={filePathRoots.join('\n')}
					placeholder="留空表示不增加通用文件路径限制"
					oninput={(event) => {
						security.resource_policy.files.path_roots = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
					}}></textarea>
			</SettingsField>
			<SettingsField
				label="只读保护路径"
				id="security-read-only-roots"
				description="每行一个已存在的本地绝对文件或目录。路径内允许读取，但写入会被拒绝，即使它位于更大的写入目录中。"
				stacked
			>
				<textarea
					id="security-read-only-roots"
					class="md-textarea md-textarea--code security-roots"
					rows="3"
					value={readOnlyRoots.join('\n')}
					placeholder="例如：C:\\Projects\\haven\\vendor"
					oninput={(event) => {
						security.resource_policy.files.read_only_roots = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
					}}></textarea>
			</SettingsField>
			<SettingsField
				label="拒绝访问路径"
				id="security-denied-paths"
				description="每行一个已存在的本地绝对文件或目录。文件本身或目录及其子项的读取和写入都会被拒绝；拒绝规则优先于所有读写授权。"
				stacked
			>
				<textarea
					id="security-denied-paths"
					class="md-textarea md-textarea--code security-roots"
					rows="3"
					value={deniedPaths.join('\n')}
					placeholder="例如：C:\\Projects\\haven\\secrets"
					oninput={(event) => {
						security.resource_policy.files.denied_paths = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
					}}></textarea>
			</SettingsField>
			<SettingsField
				label="网络域名范围"
				id="security-network-domains"
				description="可选；每行一个主机名或 *.example.com 通配范围。全局和工具级名单同时配置时必须同时匹配；该范围也会检查 HTTP 重定向。"
				stacked
			>
				<textarea
					id="security-network-domains"
					class="md-textarea md-textarea--code security-roots"
					rows="3"
					value={networkAllowedDomains.join('\n')}
					placeholder="留空表示不增加域名限制"
					oninput={(event) => {
						security.resource_policy.network.allowed_domains = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
					}}></textarea>
			</SettingsField>
			<div class="effective-write-scope" aria-live="polite">
				<strong>当前写入边界</strong>
				{#if security.resource_policy.files.write_mode === 'read_only'}
					<p>文件修改会被拒绝；下面列出的目录不会覆盖只读策略。</p>
				{:else if security.resource_policy.files.write_mode === 'unrestricted'}
					<p>当前不附加路径范围限制，仍受授权和 Windows 用户权限约束。</p>
				{:else if security.resource_policy.files.write_mode === 'roots'}
					<p>文件写入只能进入配置的目录，并继续受工具级范围限制。</p>
				{:else}
					<p>文件写入限于当前会话工作目录，或工具级规则允许的更小范围。</p>
				{/if}
				{#if writeRoots.length > 0}
					<ul aria-label="策略中的写入目录">
						{#each writeRoots as root (root)}
							<li><code>{root}</code></li>
						{/each}
					</ul>
				{:else if security.resource_policy.files.write_mode === 'workspace'}
					<p class="effective-write-scope__root">
						{security.execution_root || '启动时发现的项目根'}
					</p>
				{/if}
			</div>
		</div>
	</div>

	<div class="settings-subsection">
		<div class="subsection-heading">
			<div>
				<h3>WorkspaceWrite Shell 的 MXC 隔离</h3>
				<p>只影响 Windows 上 WorkspaceWrite 模式的 Shell 进程；FullAccess 与其它进程入口不使用 MXC。</p>
			</div>
		</div>
		<div class="policy-grid" role="radiogroup" aria-label="MXC 隔离模式">
			{#each MXC_ISOLATION_MODES as mode (mode.value)}
				<label
					class="policy-option"
					class:selected={security.mxc_isolation_mode === mode.value}
				>
					<input
						type="radio"
						name="mxc-isolation-mode"
						value={mode.value}
						checked={security.mxc_isolation_mode === mode.value}
						onchange={() => {
							security.mxc_isolation_mode = mode.value;
						}}
					/>
					<span class="policy-option__body">
						<strong>{mode.label}</strong>
						<span>{mode.detail}</span>
					</span>
				</label>
			{/each}
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
							{#if perm.effect === 'allow' && perm.resource_scope?.length}
								{#each perm.resource_scope as resourceScope, index (`${resourceScope.kind}:${index}`)}
									<span class="perm-scope"
										>资源：{permissionResourceScopeDescription(
											resourceScope,
										)}</span
									>
								{/each}
							{/if}
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
							{#if grant.effect === 'allow' && grant.resource_scope.length > 0}
								{#each grant.resource_scope as resourceScope, index (`${resourceScope.kind}:${index}`)}
									<span class="perm-scope"
										>资源：{permissionResourceScopeDescription(
											resourceScope,
										)}</span
									>
								{/each}
							{:else if grant.effect === 'deny'}
								<span class="perm-scope">拒绝适用于此授权键及匹配的下级操作</span>
							{/if}
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
	.resource-boundary-details {
		display: grid;
		gap: var(--md-sys-space-md);
		margin-top: var(--md-sys-space-md);
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
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
