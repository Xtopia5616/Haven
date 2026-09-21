<script>
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { inputElementValue, withStringValue } from '$lib/typedCallbacks.js';

	/**
	 * Permission settings presentation. The route owns persistence and command
	 * calls; this component owns the user-facing policy choices and confirmation
	 * UI for immediate permission-rule actions.
	 */
	let {
		security,
		onRevokePermission = async () => true,
		onResetPermissions = async () => true,
	} = $props();

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
	];

	const SANDBOX_MODES = [
		{
			value: 'read_only',
			label: '只读沙箱',
			detail: '禁止所有文件修改，适合只查看和分析内容。',
		},
		{
			value: 'workspace_write',
			label: '工作区可写',
			detail: '允许受控地修改工作区文件；无法隔离的命令、MCP 与技能子进程仍会拦截。',
		},
		{
			value: 'full_access',
			label: '完全访问',
			detail: '不限制文件访问范围，但确认策略和关键操作底线仍然有效。',
		},
	];

	const NETWORK_POLICIES = [
		{
			value: 'deny',
			label: '禁止网络',
			detail: '阻止所有需要联网的能力。',
		},
		{
			value: 'ask',
			label: '请求确认',
			detail: '可验证的公网请求先询问；无法约束的 MCP、技能和命令子进程仍受沙箱限制。',
		},
		{
			value: 'restricted',
			label: '受限网络',
			detail: '只允许 Haven 能验证目的地的公共网络请求。',
		},
		{
			value: 'open',
			label: '开放网络',
			detail: '不添加全局网络限制，但每个工具自己的安全检查仍会执行。',
		},
	];

	/** @type {Record<string, string>} */
	const RULE_LABELS = {
		shell: '执行本机命令',
		files: '文件操作',
		process: '进程管理',
		http: '网络请求',
		system: '系统能力',
		window: '窗口控制',
		input: '模拟输入',
		memory: '长期记忆',
		clipboard: '剪贴板',
		actions: '任务管理',
		agent: '代理协作',
		media: '媒体处理',
		schedule: '定时任务',
	};

	/** @type {Record<string, string>} */
	const OPERATION_LABELS = {
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
	let permissions = $derived(Array.isArray(security?.permissions) ? security.permissions : []);
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

	/** @param {string} key */
	function permissionRuleMeta(key) {
		const parts = String(key || '')
			.split('.')
			.filter(Boolean);
		const rootLabel = RULE_LABELS[parts[0]] || parts[0] || '未知能力';
		if (parts.length <= 1) {
			return { label: rootLabel, scope: '覆盖此能力下的全部操作' };
		}

		const operation = parts.at(-1) || '';
		const operationLabel = OPERATION_LABELS[operation] || operation;
		if (parts.length === 2) {
			return { label: `${rootLabel} · ${operationLabel}`, scope: '仅此操作' };
		}

		const group = parts.at(-2) || '';
		const groupLabel = OPERATION_LABELS[group] || group;
		return {
			label: `${rootLabel} · ${operationLabel}`,
			scope: `功能组：${groupLabel}`,
		};
	}

	function openResetDialog() {
		if (!resetPending) resetDialogOpen = true;
	}

	function closeResetDialog() {
		if (!resetPending) resetDialogOpen = false;
	}

	/** @param {string} key */
	async function revokePermission(key) {
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
			const result = await onResetPermissions?.();
			if (result !== false) resetDialogOpen = false;
		} finally {
			resetPending = false;
		}
	}
</script>

<SettingsSection
	title="权限中心"
	description="把 Haven 能做什么、什么时候询问，以及永久规则的影响范围放在一起管理。"
>
	<div class="security-summary">
		<div>
			<span class="summary-eyebrow">当前默认策略</span>
			<strong>{selectedPermissionMode.label} · {selectedPermissionMode.shortLabel}</strong>
			<p>{selectedPermissionMode.detail}</p>
		</div>
		<span class="summary-status">正在生效</span>
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
				<p>这些边界比默认策略更底层，会直接限制文件和网络能力。</p>
			</div>
		</div>
		<div class="boundary-grid">
			<div class="boundary-card">
				<SettingsField label="文件沙箱" id="security-sandbox">
					<MaterialSelect
						id="security-sandbox"
						value={security.sandbox_mode || 'workspace_write'}
						options={SANDBOX_MODES.map((mode) => ({
							value: mode.value,
							label: mode.label,
						}))}
						onChange={withStringValue((value) => {
							security.sandbox_mode = value;
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
							security.network_policy = value;
						})}
					/>
				</SettingsField>
				<p class="boundary-detail">{selectedNetworkPolicy.detail}</p>
			</div>
		</div>

		<details class="advanced-boundary" open={security.writable_roots?.length > 0}>
			<summary>
				<span>高级：自定义可写目录</span>
				<small>{security.writable_roots?.length || 0} 个目录</small>
			</summary>
			<div class="advanced-boundary__body">
				<p>留空则使用各工具自己的路径边界。每行填写一个绝对路径。</p>
				<textarea
					id="security-writable-roots"
					class="security-roots"
					rows="3"
					value={(security.writable_roots || []).join('\n')}
					placeholder="例如：C:\\Users\\me\\Projects\\haven"
					oninput={(event) => {
						security.writable_roots = inputElementValue(event)
							.split(/\r?\n/)
							.map((value) => value.trim())
							.filter(Boolean);
					}}></textarea>
			</div>
		</details>
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
					<div class="perm-row" role="listitem">
						<div
							class="perm-indicator"
							class:deny={perm.effect === 'deny'}
							aria-hidden="true"
						>
							{perm.effect === 'deny' ? '!' : '✓'}
						</div>
						<div class="perm-copy">
							<strong>{meta.label}</strong>
							<span class="perm-scope">{meta.scope}</span>
							<code class="perm-key">{perm.key}</code>
						</div>
						<span class="perm-effect" class:deny={perm.effect === 'deny'}>
							{perm.effect === 'deny' ? '始终拒绝' : '始终允许'}
						</span>
						<MaterialButton
							variant="text"
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
				variant="text"
				className="permission-reset"
				label="清除所有规则"
				onclick={openResetDialog}
				disabled={pendingRule !== '' || resetPending}
			/>
		{:else}
			<div class="empty-rules">
				<strong>还没有永久规则</strong>
				<p>需要长期记住某个允许或拒绝决定时，可以在权限确认弹窗中选择对应选项。</p>
			</div>
		{/if}
	</div>
</SettingsSection>

<MaterialDialog open={resetDialogOpen} title="清除永久规则" onClose={closeResetDialog}>
	{#snippet children()}
		<p class="reset-dialog-copy">
			这会清除所有“始终允许”和“始终拒绝”规则。之后 Haven
			会按当前默认策略重新询问，正在使用的安全边界不会改变。
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
	.empty-rules p,
	.reset-dialog-copy {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.policy-grid {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
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
		grid-template-columns: repeat(2, minmax(0, 1fr));
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
	.advanced-boundary summary {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 700;
		cursor: pointer;
		list-style-position: inside;
	}
	.advanced-boundary summary small {
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 400;
	}
	.advanced-boundary__body {
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-md);
	}
	.security-roots {
		box-sizing: border-box;
		width: 100%;
		margin-top: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-lowest);
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-family: var(--md-sys-typescale-mono);
		resize: vertical;
	}
	.security-roots:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
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
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.md-btn.perm-revoke:hover) {
		color: var(--md-sys-color-error);
	}
	:global(.md-btn.permission-reset) {
		margin-top: var(--md-sys-space-sm);
		color: var(--md-sys-color-error);
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

	@media (max-width: 820px) {
		.policy-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (max-width: 640px) {
		.security-summary,
		.perm-row {
			align-items: stretch;
			flex-direction: column;
		}
		.summary-status,
		.rules-count {
			align-self: flex-start;
		}
		.policy-grid,
		.boundary-grid {
			grid-template-columns: 1fr;
		}
		.perm-effect,
		:global(.md-btn.perm-revoke) {
			align-self: flex-start;
		}
	}
</style>
