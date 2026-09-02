<script>
	import { onDestroy } from 'svelte';
	import { themeStore } from '$lib/themeStore.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
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
	<div class="section">
		<h2>Hotkeys</h2>
		<div class="form-row">
			<label for="hotkey-binding">Key Binding</label>
			<HotkeyInput
				id="hotkey-binding"
				value={hotkeyBinding}
				onChange={withStringValue((v) => onHotkeyBindingChange(v))}
			/>
		</div>
		<div class="form-row">
			<label for="hotkey-mode">Mode</label>
			<MaterialSelect
				id="hotkey-mode"
				value={hotkeyMode}
				options={[
					{ value: 'toggle', label: 'Toggle (press to start/stop)' },
					{ value: 'hold', label: 'Hold (push-to-talk)' },
				]}
				onChange={withStringValue((v) => onHotkeyModeChange(v))}
			/>
		</div>
	</div>

	<div class="section">
		<h2>Session &amp; Concurrency</h2>
		<p class="model-hint">
			Max Concurrent 控制同时运行的会话数；LLM Per-Endpoint Concurrency
			限制每个模型端点（角色）同时在途的请求数。后者低于前者时，超出上限的模型请求会排队等待，避免多个会话同时请求同一服务商触发限流（429）。
		</p>
		<div class="form-row">
			<label for="session-max-concurrent">Max Concurrent</label>
			<MaterialNumberField
				id="session-max-concurrent"
				value={session.max_concurrent}
				min={1}
				max={10}
				onChange={withNumberValue((v) => {
					session.max_concurrent = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="llm-max-concurrent-requests">LLM Per-Endpoint Concurrency</label>
			<MaterialNumberField
				id="llm-max-concurrent-requests"
				value={llmConfig.max_concurrent_requests}
				min={1}
				max={16}
				onChange={withNumberValue((v) => {
					llmConfig.max_concurrent_requests = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="session-max-steps">Max Steps</label>
			<MaterialNumberField
				id="session-max-steps"
				value={session.max_steps}
				min={1}
				max={100}
				onChange={withNumberValue((v) => {
					session.max_steps = v;
				})}
			/>
		</div>
	</div>

	<div class="section">
		<h2>Agent Shell</h2>
		<p class="model-hint">
			Agent 的 shell 工具默认使用的命令行解释器。模型仍可在调用时通过 shell 参数临时指定其他
			shell（cmd / powershell / pwsh）。
		</p>
		<div class="form-row">
			<label for="default-shell">Default Shell</label>
			<MaterialSelect
				id="default-shell"
				value={defaultShell}
				options={shellOptions()}
				onChange={withStringValue((v) => onDefaultShellChange(v))}
			/>
		</div>
		{#if defaultShell === 'pwsh' && shellAvailable.pwsh === false}
			<div class="shell-warning">
				<p>未检测到 PowerShell 7（pwsh），命令将无法执行。请先安装：</p>
				<code>winget install Microsoft.PowerShell</code>
			</div>
		{/if}
	</div>

	<div class="section">
		<h2>Memory</h2>
		<div class="form-row">
			<label for="memory-window-size">Window Size</label>
			<MaterialNumberField
				id="memory-window-size"
				value={memory.session_window_size}
				min={10}
				max={500}
				onChange={withNumberValue((v) => {
					memory.session_window_size = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="memory-retention">Retention (days)</label>
			<MaterialNumberField
				id="memory-retention"
				value={memory.history_retention_days}
				min={1}
				max={365}
				onChange={withNumberValue((v) => {
					memory.history_retention_days = v;
				})}
			/>
		</div>
		<h3 class="model-group-heading">Maintenance</h3>
		<p class="model-hint">维护会清理重复、敏感、过期的事实与残留向量。</p>
		<div class="form-row">
			<button
				class="md-btn"
				onclick={() => onRunMaintenance()}
				disabled={memoryMaintenance.running}
			>
				{memoryMaintenance.running ? 'Running…' : 'Run Memory Maintenance'}
			</button>
			{#if memoryMaintenance.lastCount !== null}
				<span class="recall-hint">上次清理 {memoryMaintenance.lastCount} 项</span>
			{/if}
		</div>
	</div>

	<div class="section appearance-section">
		<h2>Appearance</h2>
		<div class="form-row">
			<span class="form-label">Theme</span>
			<div class="theme-toggle-row" role="radiogroup" aria-label="Theme">
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'light'}
					class:md-btn--filled={currentTheme !== 'light'}
					role="radio"
					aria-checked={currentTheme === 'light'}
					onclick={() => themeStore.setTheme('light')}>Light</button
				>
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'dark'}
					class:md-btn--filled={currentTheme !== 'dark'}
					role="radio"
					aria-checked={currentTheme === 'dark'}
					onclick={() => themeStore.setTheme('dark')}>Dark</button
				>
			</div>
		</div>
		<div class="form-row">
			<span class="form-label">Accent Color</span>
			<div class="accent-picker" role="radiogroup" aria-label="Accent color">
				{#each Object.entries(themeStore.presets) as [key, preset]}
					<button
						class="md-btn"
						class:accent-swatch-selected={accent === key}
						style="background: {preset.hex}; color: {contrastText(
							preset.hex,
						)}; --_btn-state: {contrastText(
							preset.hex,
						)}; border: 2px solid transparent; border-color: {accent === key
							? contrastText(preset.hex)
							: 'transparent'}"
						role="radio"
						aria-checked={accent === key}
						aria-label="{preset.label} {preset.hex}"
						onclick={() => {
							accent = key;
							themeStore.setAccent(key);
						}}>{preset.label}</button
					>
				{/each}
				<button
					class="md-btn md-btn--filled"
					class:md-btn--outlined={accent.startsWith('#') || accent.startsWith('custom:')}
					role="radio"
					aria-checked={accent.startsWith('#') || accent.startsWith('custom:')}
					aria-label="Custom hex color"
				>
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
				</button>
			</div>
		</div>
	</div>

	<div class="section">
		<h2>Security</h2>
		<div class="form-row">
			<label for="security-mode">Confirmation Mode</label>
			<MaterialSelect
				id="security-mode"
				value={security.confirmation_mode}
				options={[
					{ value: 'ask', label: 'Ask (by risk threshold)' },
					{ value: 'paranoid', label: 'Paranoid (all non-safe)' },
					{ value: 'autopilot', label: 'Autopilot (never ask)' },
				]}
				onChange={withStringValue((v) => {
					security.confirmation_mode = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="security-min-level">Minimum Confirmation Level</label>
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
		</div>
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
						<button
							type="button"
							class="perm-revoke"
							onclick={() => revokePermission(perm.key)}>撤销</button
						>
					</div>
				{/each}
			</div>
		{:else}
			<p class="model-hint">
				暂无永久权限。确认弹窗中选「始终允许 / 始终拒绝」后会出现在这里。
			</p>
		{/if}
	</div>

	<div class="section notification-section">
		<h2>Notifications</h2>
		<div class="notify-grid-header">
			<span class="switch-label"></span><span class="switch-label">In-App Toast</span><span
				class="switch-label">Windows</span
			>
		</div>
		{#each [{ key: 'session_created', label: 'Session Start' }, { key: 'session_completed', label: 'Session Complete' }, { key: 'session_paused', label: 'Session Paused' }, { key: 'session_resumed', label: 'Session Resumed' }, { key: 'session_error', label: 'Session Error' }] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch
					checked={notification[ev.key].in_app}
					onChange={withBooleanValue((v) => {
						notification[ev.key].in_app = v;
					})}
				/>
				<MaterialSwitch
					checked={notification[ev.key].windows}
					onChange={withBooleanValue((v) => {
						notification[ev.key].windows = v;
					})}
				/>
			</div>
		{/each}
		<p class="model-hint">
			Agent 通过 notify 工具发出的通知始终开启（应用内 + Windows），不受上表开关控制。
		</p>
	</div>

	<div class="section log-section">
		<div class="llm-head">
			<h2>Logging</h2>
			<button
				class="md-btn md-btn--outlined"
				onclick={() => onOpenLogViewer()}
				disabled={logView.loading}>查看日志</button
			>
		</div>
		<div class="form-row switch-row">
			<span class="switch-label">File Logging</span><MaterialSwitch
				checked={log.file_enabled}
				onChange={withBooleanValue((v) => {
					log.file_enabled = v;
				})}
			/>
		</div>
		<div class="form-row">
			<label for="log-level">Log Level</label>
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
	</div>

	<div class="section autostart-section">
		<h2>Autostart</h2>
		<div class="form-row autostart-row">
			<span class="autostart-label">Launch Haven on system startup</span><MaterialSwitch
				checked={autostartEnabled}
				onChange={withBooleanValue((v) => onAutostartChange(v))}
			/>
		</div>
	</div>
</div>

<style>
	.section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-lg);
	}
	.section h2 {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: 1px;
		margin-bottom: var(--md-sys-space-lg);
	}
	.model-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-sm));
		margin-bottom: var(--md-sys-space-md);
	}
	.model-group-heading {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-lg);
		margin-bottom: var(--md-sys-space-sm);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}
	.form-row {
		display: flex;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
		gap: var(--md-sys-space-md);
	}
	.form-row label,
	.form-row .form-label {
		width: 120px;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		flex-shrink: 0;
	}
	.switch-row,
	.autostart-section .autostart-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label,
	.autostart-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.shell-warning {
		margin-top: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-medium);
		background: color-mix(in srgb, var(--md-sys-color-error) 10%, transparent);
		color: var(--md-sys-color-on-surface);
		font-size: 12px;
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
		font-size: 12px;
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
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.perm-effect {
		font-size: 11px;
		font-weight: 700;
		color: var(--md-sys-color-primary);
		text-transform: capitalize;
	}
	.perm-effect.deny {
		color: var(--md-sys-color-error);
	}
	.perm-revoke {
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font: inherit;
		font-size: 12px;
		cursor: pointer;
		padding: 2px 6px;
	}
	.perm-revoke:hover {
		color: var(--md-sys-color-error);
	}
	.notify-grid-header,
	.notify-grid-row {
		display: grid;
		grid-template-columns: 1fr auto auto;
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
		font-size: 11px;
		text-transform: uppercase;
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
		font-size: 13px;
	}
	.theme-toggle-row,
	.accent-picker {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
		flex-wrap: wrap;
	}
	.accent-swatch-selected {
		outline: 2px solid var(--md-sys-color-on-surface);
		outline-offset: -2px;
	}
	.custom-hex-input {
		width: 84px;
		font-family: var(--md-sys-typescale-mono);
		font-size: 14px;
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
		.form-row label,
		.form-row .form-label {
			width: auto;
			flex-shrink: 1;
		}
		.switch-row {
			align-items: flex-start;
		}
		.switch-label {
			padding-top: 6px;
		}
	}
</style>
