<script>
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { inputElementValue, withStringValue } from '$lib/typedCallbacks.js';

	/**
	 * Security settings have their own view because approval experience,
	 * technical boundaries, and persisted rules are one policy surface. The
	 * parent settings page owns saving; this component only edits the shared
	 * security snapshot and delegates rule mutations to the command boundary.
	 */
	let {
		security,
		onRevokePermission = async () => {},
		onResetPermissions = async () => {},
	} = $props();

	const PERMISSION_MODES = [
		{
			value: 'default',
			label: '默认',
			detail: '普通只读操作自动完成；敏感读取、出网和编辑等操作按边界询问',
		},
		{
			value: 'plan',
			label: '计划',
			detail: '仅允许只读分析，任何修改或副作用都会被拦截',
		},
		{
			value: 'auto_edit',
			label: '自动编辑',
			detail: '仅自动接受明确的工作区文件编辑；命令、网络、界面和高风险操作仍需确认',
		},
		{
			value: 'autonomous',
			label: '自动',
			detail: '非关键操作自动执行，高风险与关键操作仍需确认',
		},
	];

	const SANDBOX_MODES = [
		{ value: 'read_only', label: '只读沙箱' },
		{ value: 'workspace_write', label: '工作区可写' },
		{ value: 'full_access', label: '完全访问' },
	];

	const NETWORK_POLICIES = [
		{ value: 'deny', label: '禁止网络' },
		{ value: 'restricted', label: '受限网络' },
		{ value: 'open', label: '开放网络' },
	];

	/** @param {string} key */
	function permissionRuleLabel(key) {
		/** @type {Record<string, string>} */
		const labels = {
			shell: '本机命令',
			files: '文件操作',
			'files.delete': '删除文件',
			process: '进程管理',
			http: '网络请求',
			system: '系统设置',
			window: '窗口控制',
			input: '模拟输入',
			memory: '长期记忆',
		};
		return labels[key] || key;
	}

	/** @param {string} key */
	async function revokePermission(key) {
		await onRevokePermission(key);
	}

	async function resetPermissions() {
		await onResetPermissions();
	}
</script>

<SettingsSection title="权限中心">
	<SettingsField label="默认策略" id="security-mode">
		<MaterialSelect
			id="security-mode"
			value={security.permission_mode}
			options={PERMISSION_MODES.map((mode) => ({ value: mode.value, label: mode.label }))}
			onChange={withStringValue((v) => {
				security.permission_mode = v;
			})}
		/>
	</SettingsField>
	{#if PERMISSION_MODES.find((mode) => mode.value === security.permission_mode)}
		<p class="permission-mode-detail">
			{PERMISSION_MODES.find((mode) => mode.value === security.permission_mode)?.detail}
		</p>
	{/if}
	<div class="permission-boundary-grid">
		<SettingsField label="文件沙箱" id="security-sandbox">
			<MaterialSelect
				id="security-sandbox"
				value={security.sandbox_mode || 'workspace_write'}
				options={SANDBOX_MODES}
				onChange={withStringValue((v) => {
					security.sandbox_mode = v;
				})}
			/>
		</SettingsField>
		<SettingsField label="网络策略" id="security-network">
			<MaterialSelect
				id="security-network"
				value={security.network_policy || 'restricted'}
				options={NETWORK_POLICIES}
				onChange={withStringValue((v) => {
					security.network_policy = v;
				})}
			/>
		</SettingsField>
	</div>
	<SettingsField
		label="可写根目录"
		id="security-writable-roots"
		description="可选；每行一个绝对路径，留空则使用各工具自己的路径边界。"
		stacked
	>
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
			}}
		></textarea>
	</SettingsField>
	<p class="model-hint">
		沙箱与确认是两道独立边界：只读沙箱会直接拦截修改，工作区可写模式会拦截无法隔离的命令、MCP 与技能子进程；受限网络会拦截无法检查目的地的出网。
	</p>
	<div class="permission-callout">
		<div class="permission-callout-icon" aria-hidden="true">✓</div>
		<div>
			<strong>安全边界始终有效</strong>
			<p>永久拒绝、禁用的操作、路径沙箱和关键系统操作不会被默认策略或会话允许绕过。</p>
		</div>
	</div>
	{#if security.permissions.length > 0}
		<div class="perm-list">
			<div class="perm-list-title">已保存的规则</div>
			{#each security.permissions as perm (perm.key)}
				<div class="perm-row">
					<div class="perm-copy">
						<strong>{permissionRuleLabel(perm.key)}</strong>
						<code class="perm-key">{perm.key}</code>
					</div>
					<span class="perm-effect" class:deny={perm.effect === 'deny'}
						>{perm.effect === 'deny' ? '始终拒绝' : '始终允许'}</span
					>
					<MaterialButton
						variant="text"
						className="perm-revoke"
						label="撤销"
						onclick={() => revokePermission(perm.key)}
					/>
				</div>
			{/each}
			<MaterialButton
				variant="text"
				className="permission-reset"
				label="清除所有规则"
				onclick={resetPermissions}
			/>
		</div>
	{:else}
		<p class="model-hint">
			还没有覆盖规则。你可以在权限弹窗中选择「始终允许」或「始终拒绝」，精确控制某个工具或操作。
		</p>
	{/if}
</SettingsSection>

<style>
	.permission-mode-detail {
		margin: calc(-1 * var(--md-sys-space-sm)) 0 var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.permission-boundary-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.security-roots {
		box-sizing: border-box;
		width: min(100%, var(--md-comp-settings-control-width));
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		font: inherit;
		font-family: var(--md-sys-typescale-mono);
		resize: vertical;
	}
	.security-roots:focus {
		outline: none;
		border-color: var(--md-sys-color-primary);
	}
	.permission-callout {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	.permission-callout-icon {
		display: grid;
		place-items: center;
		width: 24px;
		height: 24px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-weight: 700;
	}
	.permission-callout strong,
	.permission-callout p {
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.permission-callout p {
		margin: var(--md-sys-space-2xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.perm-list {
		margin-top: var(--md-sys-space-md);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.perm-list-title {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
	}
	.perm-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-extra-small);
		background: var(--md-sys-color-surface-container-low, rgba(0, 0, 0, 0.03));
	}
	.perm-key {
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.perm-copy {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		flex: 1;
	}
	.perm-copy strong {
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.perm-effect {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-primary);
		text-transform: capitalize;
	}
	.perm-effect.deny {
		color: var(--md-sys-color-error);
	}
	:global(.md-btn.perm-revoke) {
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font: inherit;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		cursor: pointer;
		padding: 2px 6px;
	}
	:global(.md-btn.perm-revoke:hover) {
		color: var(--md-sys-color-error);
	}
	:global(.md-btn.permission-reset) {
		align-self: flex-start;
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-error);
	}
	@media (max-width: 720px) {
		.permission-boundary-grid {
			grid-template-columns: 1fr;
		}
	}
</style>
