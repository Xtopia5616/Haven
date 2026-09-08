<script>
	import { onDestroy } from 'svelte';
	import { themeStore } from '$lib/themeStore.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import HotkeyInput from '$lib/HotkeyInput.svelte';
	import {
		inputElementValue,
		withBooleanValue,
		withNumberValue,
		withStringValue,
	} from '$lib/typedCallbacks.js';

	/**
	 * General settings presentation. The settings page owns the mutable
	 * snapshot/save boundary; this component only edits the shared objects and
	 * reports primitive field changes through callbacks.
	 */
	let {
		hotkeyMode,
		hotkeyBinding,
		llmConfig,
		session,
		defaultShell,
		shellAvailable,
		memory,
		memoryMaintenance,
		security,
		notification,
		log,
		logView,
		autostartEnabled,
		onHotkeyModeChange = () => {},
		onHotkeyBindingChange = () => {},
		onDefaultShellChange = () => {},
		onAutostartChange = () => {},
		onRunMaintenance = () => {},
		onOpenLogViewer = () => {},
		onRevokePermission = async () => {},
	} = $props();

	const SHELL_BASE_OPTIONS = [
		{ value: 'cmd', label: 'cmd.exe（命令提示符）' },
		{ value: 'powershell', label: 'Windows PowerShell（系统自带）' },
		{ value: 'pwsh', label: 'PowerShell 7（pwsh）' },
	];

	function shellOptions() {
		return SHELL_BASE_OPTIONS.map((o) =>
			o.value === 'pwsh' && shellAvailable.pwsh === false
				? { ...o, label: `${o.label}（未安装）` }
				: o,
		);
	}

	let currentTheme = $state(themeStore.currentTheme);
	let accent = $state(themeStore.currentAccent);
	let customAccentHex = $state(themeStore.isPreset ? '#2C5090' : themeStore.accentColor);
	const unsubscribeTheme = themeStore.subscribe((v) => {
		currentTheme = v.theme;
	});

	onDestroy(() => unsubscribeTheme());

	/** @param {string} hex */
	function contrastText(hex) {
		const r = parseInt(hex.slice(1, 3), 16);
		const g = parseInt(hex.slice(3, 5), 16);
		const b = parseInt(hex.slice(5, 7), 16);
		const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
		return lum > 0.5 ? '#000000' : '#ffffff';
	}

	/** @param {string} key */
	async function revokePermission(key) {
		await onRevokePermission(key);
	}
</script>

<div class="settings-general">
	<SettingsSection title="快捷键">
		<SettingsField label="快捷键" id="hotkey-binding">
			<HotkeyInput
				id="hotkey-binding"
				value={hotkeyBinding}
				onChange={withStringValue((v) => onHotkeyBindingChange(v))}
			/>
		</SettingsField>
		<SettingsField label="录音模式" id="hotkey-mode">
			<MaterialSelect
				id="hotkey-mode"
				value={hotkeyMode}
				options={[
					{ value: 'toggle', label: '切换（按键开始 / 停止）' },
					{ value: 'hold', label: '按住说话' },
				]}
				onChange={withStringValue((v) => onHotkeyModeChange(v))}
			/>
		</SettingsField>
	</SettingsSection>

	<SettingsSection title="会话与并发">
		<p class="model-hint">
			Max Concurrent 控制同时运行的会话数；LLM Per-Endpoint Concurrency
			限制每个模型端点（角色）同时在途的请求数。后者低于前者时，超出上限的模型请求会排队等待，避免多个会话同时请求同一服务商触发限流（429）。
		</p>
		<SettingsField label="最大并发会话" id="session-max-concurrent">
			<MaterialNumberField
				id="session-max-concurrent"
				value={session.max_concurrent}
				min={1}
				max={10}
				onChange={withNumberValue((v) => {
					session.max_concurrent = v;
				})}
			/>
		</SettingsField>
		<SettingsField label="模型端点并发" id="llm-max-concurrent-requests">
			<MaterialNumberField
				id="llm-max-concurrent-requests"
				value={llmConfig.max_concurrent_requests}
				min={1}
				max={16}
				onChange={withNumberValue((v) => {
					llmConfig.max_concurrent_requests = v;
				})}
			/>
		</SettingsField>
		<SettingsField label="最大步骤数" id="session-max-steps">
			<MaterialNumberField
				id="session-max-steps"
				value={session.max_steps}
				min={1}
				max={100}
				onChange={withNumberValue((v) => {
					session.max_steps = v;
				})}
			/>
		</SettingsField>
	</SettingsSection>

	<SettingsSection title="命令行工具">
		<p class="model-hint">
			Agent 的 shell 工具默认使用的命令行解释器。模型仍可在调用时通过 shell 参数临时指定其他
			shell（cmd / powershell / pwsh）。
		</p>
		<SettingsField label="默认 Shell" id="default-shell">
			<MaterialSelect
				id="default-shell"
				value={defaultShell}
				options={shellOptions()}
				onChange={withStringValue((v) => onDefaultShellChange(v))}
			/>
		</SettingsField>
		{#if defaultShell === 'pwsh' && shellAvailable.pwsh === false}
			<div class="shell-warning">
				<p>未检测到 PowerShell 7（pwsh），命令将无法执行。请先安装：</p>
				<code>winget install Microsoft.PowerShell</code>
			</div>
		{/if}
	</SettingsSection>

	<SettingsSection title="记忆">
		<SettingsField label="窗口大小" id="memory-window-size">
			<MaterialNumberField
				id="memory-window-size"
				value={memory.session_window_size}
				min={10}
				max={500}
				onChange={withNumberValue((v) => {
					memory.session_window_size = v;
				})}
			/>
		</SettingsField>
		<SettingsField label="保留天数" id="memory-retention">
			<MaterialNumberField
				id="memory-retention"
				value={memory.history_retention_days}
				min={1}
				max={365}
				onChange={withNumberValue((v) => {
					memory.history_retention_days = v;
				})}
			/>
		</SettingsField>
		<h3 class="model-group-heading">维护</h3>
		<p class="model-hint">维护会清理重复、敏感、过期的事实与残留向量。</p>
		<div class="form-row">
			<MaterialButton
				variant="text"
				label={memoryMaintenance.running ? '运行中…' : '执行记忆维护'}
				onclick={() => onRunMaintenance()}
				disabled={memoryMaintenance.running}
			/>
			{#if memoryMaintenance.lastCount !== null}
				<span class="recall-hint">上次清理 {memoryMaintenance.lastCount} 项</span>
			{/if}
		</div>
	</SettingsSection>

	<SettingsSection title="外观" className="appearance-section">
		<SettingsField label="主题">
			<div class="theme-toggle-row" role="radiogroup" aria-label="主题">
				<MaterialButton
					variant={currentTheme === 'light' ? 'filled' : 'outlined'}
					label="浅色"
					role="radio"
					ariaChecked={currentTheme === 'light'}
					onclick={() => themeStore.setTheme('light')}
				/>
				<MaterialButton
					variant={currentTheme === 'dark' ? 'filled' : 'outlined'}
					label="深色"
					role="radio"
					ariaChecked={currentTheme === 'dark'}
					onclick={() => themeStore.setTheme('dark')}
				/>
			</div>
		</SettingsField>
		<SettingsField label="强调色">
			<div class="accent-picker" aria-label="强调色">
				{#each Object.entries(themeStore.presets) as [key, preset]}
					<MaterialButton
						variant="filled"
						label={preset.label}
						className={`accent-swatch ${accent === key ? 'accent-swatch-selected' : ''}`}
						style={`background: ${preset.hex}; color: ${contrastText(
							preset.hex,
						)}; --_btn-state: ${contrastText(
							preset.hex,
						)}; border: 2px solid transparent; border-color: ${accent === key
							? contrastText(preset.hex)
							: 'transparent'}`}
						role="radio"
						ariaChecked={accent === key}
						ariaLabel={`${preset.label} ${preset.hex}`}
						onclick={() => {
							accent = key;
							themeStore.setAccent(key);
						}}
					/>
				{/each}
				<label class="accent-custom" for="custom-accent">
					<span>Custom</span>
					<input
						id="custom-accent"
						type="text"
						class="custom-hex-input"
						placeholder="#RRGGBB"
						maxlength="7"
						value={customAccentHex}
						autocomplete="off"
						oninput={(e) => {
							const val = inputElementValue(e);
							customAccentHex = val;
							if (/^#[0-9a-f]{6}$/i.test(val)) {
								accent = val;
								themeStore.setAccent(val);
							}
						}}
					/>
				</label>
			</div>
		</SettingsField>
	</SettingsSection>

	<SettingsSection title="安全">
		<SettingsField label="确认模式" id="security-mode">
			<MaterialSelect
				id="security-mode"
				value={security.confirmation_mode}
				options={[
					{ value: 'ask', label: '询问（按风险阈值）' },
					{ value: 'paranoid', label: '谨慎（所有非安全操作）' },
					{ value: 'autopilot', label: '自动驾驶（不询问）' },
				]}
				onChange={withStringValue((v) => {
					security.confirmation_mode = v;
				})}
			/>
		</SettingsField>
		<SettingsField label="最低确认级别" id="security-min-level">
			<MaterialSelect
				id="security-min-level"
				value={security.min_risk_level}
				options={[
					{ value: 'safe', label: 'None (all auto-approved)' },
					{ value: 'low', label: 'Low & above' },
					{ value: 'medium', label: 'Medium & above' },
					{ value: 'high', label: 'High & above' },
					{ value: 'critical', label: 'Critical only' },
				]}
				onChange={withStringValue((v) => {
					security.min_risk_level = v;
				})}
			/>
		</SettingsField>
		<p class="model-hint">
			仅 Ask 模式使用风险阈值。永久允许/拒绝优先于阈值；禁用操作与路径沙箱始终拦截。Autopilot
			仍会执行永久拒绝。
		</p>
		{#if security.permissions.length > 0}
			<div class="perm-list">
				<div class="perm-list-title">永久权限</div>
				{#each security.permissions as perm (perm.key)}
					<div class="perm-row">
						<code class="perm-key">{perm.key}</code>
						<span class="perm-effect" class:deny={perm.effect === 'deny'}
							>{perm.effect === 'deny' ? '拒绝' : '允许'}</span
						>
						<MaterialButton
							variant="text"
							className="perm-revoke"
							label="撤销"
							onclick={() => revokePermission(perm.key)}
						/>
					</div>
				{/each}
			</div>
		{:else}
			<p class="model-hint">
				暂无永久权限。确认弹窗中选「始终允许 / 始终拒绝」后会出现在这里。
			</p>
		{/if}
	</SettingsSection>

	<SettingsSection title="通知" className="notification-section">
		<div class="notify-grid-header">
			<span class="switch-label"></span><span class="switch-label">应用内提示</span><span
				class="switch-label">Windows 通知</span
			>
		</div>
		{#each [{ key: 'session_created', label: '会话开始' }, { key: 'session_completed', label: '会话完成' }, { key: 'session_paused', label: '会话暂停' }, { key: 'session_resumed', label: '会话恢复' }, { key: 'session_error', label: '会话出错' }] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch
					checked={notification[ev.key].in_app}
					ariaLabel={`${ev.label}应用内提示`}
					onChange={withBooleanValue((v) => {
						notification[ev.key].in_app = v;
					})}
				/>
				<MaterialSwitch
					checked={notification[ev.key].windows}
					ariaLabel={`${ev.label} Windows 通知`}
					onChange={withBooleanValue((v) => {
						notification[ev.key].windows = v;
					})}
				/>
			</div>
		{/each}
		<p class="model-hint">
			Agent 通过 notify 工具发出的通知始终开启（应用内 + Windows），不受上表开关控制。
		</p>
	</SettingsSection>

	<SettingsSection className="log-section">
		<div class="llm-head">
			<h2>日志</h2>
			<MaterialButton
				variant="outlined"
				label="查看日志"
				onclick={() => onOpenLogViewer()}
				disabled={logView.loading}
			/>
		</div>
		<div class="form-row switch-row">
			<span class="switch-label">文件日志</span><MaterialSwitch
				checked={log.file_enabled}
				ariaLabel="切换文件日志"
				onChange={withBooleanValue((v) => {
					log.file_enabled = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="log-level">日志级别</label>
			<MaterialSelect
				id="log-level"
				value={log.level}
				options={[
					{ value: 'trace', label: 'Trace' },
					{ value: 'debug', label: 'Debug' },
					{ value: 'info', label: 'Info' },
					{ value: 'warn', label: 'Warn' },
					{ value: 'error', label: 'Error' },
				]}
				onChange={withStringValue((v) => {
					log.level = v;
				})}
			/>
		</div>
		<p class="model-hint">
			日志级别与文件输出仅作用于后端（tracing）；前端开发日志仍按 DEV/PROD 门控。
		</p>
	</SettingsSection>

	<SettingsSection title="自动启动" className="autostart-section">
		<SettingsField label="开机时启动 Haven" className="autostart-row">
			<MaterialSwitch
				checked={autostartEnabled}
				ariaLabel="切换开机启动 Haven"
				onChange={withBooleanValue((v) => onAutostartChange(v))}
			/>
		</SettingsField>
	</SettingsSection>
</div>

<style>
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.model-group-heading {
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-sm);
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.form-row {
		display: flex;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
		gap: var(--md-sys-space-md);
	}
	.form-row label {
		width: var(--md-comp-settings-label-width);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		flex-shrink: 0;
	}
	.form-row > :global(.md-number-field),
	.form-row > :global(.md-select-container),
	.form-row > :global(.hotkey-input-wrap) {
		width: min(100%, var(--md-comp-settings-control-width));
		flex: 0 1 var(--md-comp-settings-control-width);
		min-width: 0;
	}
	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.shell-warning {
		margin-top: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-medium);
		background: color-mix(in srgb, var(--md-sys-color-error) 10%, transparent);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.shell-warning p {
		margin: 0 0 var(--md-sys-space-xs);
	}
	.shell-warning code {
		display: inline-block;
		padding: 2px 8px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		font-family: var(--md-sys-typescale-mono);
		user-select: all;
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
		flex: 1;
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
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
	.notify-grid-header,
	.notify-grid-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) var(--md-comp-switch-width) var(
				--md-comp-switch-width
			);
		gap: var(--md-sys-space-md);
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.notify-grid-header {
		padding-bottom: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		margin-bottom: var(--md-sys-space-md);
	}
	.notify-grid-header .switch-label {
		font-weight: 600;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head h2 {
		margin: 0;
	}
	.recall-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.theme-toggle-row,
	.accent-picker {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
		flex-wrap: wrap;
	}
	:global(.md-btn.accent-swatch-selected) {
		outline: 2px solid var(--md-sys-color-on-surface);
		outline-offset: -2px;
	}
	.accent-custom {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		min-height: var(--md-comp-button-height);
		padding: 0 var(--md-sys-space-sm);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
	}
	.custom-hex-input {
		width: 84px;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		background: transparent;
		border: none;
		outline: none;
		color: inherit;
		padding: 0;
		text-align: center;
	}
	.custom-hex-input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
	@media (max-width: 700px) {
		.form-row {
			flex-direction: column;
			align-items: stretch;
			gap: var(--md-sys-space-xs);
		}
		.form-row label {
			width: auto;
			flex-shrink: 1;
		}
		.form-row > :global(.md-number-field),
		.form-row > :global(.md-select-container),
		.form-row > :global(.hotkey-input-wrap) {
			width: min(100%, var(--md-comp-settings-control-width));
			flex: 0 1 auto;
		}
		.switch-row {
			flex-direction: row;
			align-items: center;
			justify-content: space-between;
		}
		.switch-label {
			flex: 1;
			min-width: 0;
			padding-top: 0;
		}
	}
</style>
