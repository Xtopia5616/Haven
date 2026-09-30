<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import HotkeyInput from '$lib/HotkeyInput.svelte';
	import { withBooleanValue, withNumberValue, withStringValue } from '$lib/typedCallbacks.ts';
	import type {
		HotkeyModeInput,
		MemoryConfigInput,
		SessionConfigInput,
		ShellChoiceInput,
	} from '$lib/contracts/generatedCommands.ts';

	type SessionDraft = Required<
		Pick<
			SessionConfigInput,
			'max_concurrent' | 'max_steps' | 'history_retention_days' | 'session_max_steps'
		>
	>;
	type MemoryDraft = Required<
		Pick<MemoryConfigInput, 'session_window_size' | 'fact_inference_enabled'>
	>;
	interface Props {
		hotkeyMode: HotkeyModeInput;
		hotkeyBinding: string;
		session: SessionDraft;
		defaultShell: ShellChoiceInput;
		shellAvailable: { cmd: boolean; powershell: boolean; pwsh: boolean };
		memory: MemoryDraft;
		memoryMaintenance: { running: boolean; lastCount: number | null };
		onHotkeyModeChange?: (value: HotkeyModeInput) => void;
		onHotkeyBindingChange?: (value: string) => void;
		onDefaultShellChange?: (value: ShellChoiceInput) => void;
		onRunMaintenance?: () => void;
	}

	let {
		hotkeyMode,
		hotkeyBinding,
		session,
		defaultShell,
		shellAvailable,
		memory,
		memoryMaintenance,
		onHotkeyModeChange = () => {},
		onHotkeyBindingChange = () => {},
		onDefaultShellChange = () => {},
		onRunMaintenance = () => {},
	}: Props = $props();

	const SHELL_OPTIONS = [
		{ value: 'cmd', label: '命令提示符（cmd.exe）' },
		{ value: 'powershell', label: 'Windows PowerShell' },
		{ value: 'pwsh', label: 'PowerShell 7（pwsh）' },
	] as const;

	function shellOptions() {
		return SHELL_OPTIONS.map((option) =>
			option.value === 'pwsh' && shellAvailable.pwsh === false
				? { ...option, label: `${option.label}（未安装）` }
				: option,
		);
	}
</script>

<div class="settings-behavior">
	<SettingsSection title="语音快捷键" description="决定如何通过键盘开始和结束录音。">
		<SettingsField label="录音快捷键" id="hotkey-binding">
			<HotkeyInput
				id="hotkey-binding"
				value={hotkeyBinding}
				onChange={withStringValue((value) => onHotkeyBindingChange(value))}
			/>
		</SettingsField>
		<SettingsField label="录音模式" id="hotkey-mode">
			<MaterialSelect
				id="hotkey-mode"
				value={hotkeyMode}
				options={[
					{ value: 'toggle', label: '按一次开始，再按一次停止' },
					{ value: 'hold', label: '按住说话' },
				]}
				onChange={withStringValue((value) => onHotkeyModeChange(value as HotkeyModeInput))}
			/>
		</SettingsField>
	</SettingsSection>

	<SettingsSection
		title="会话与执行"
		description="控制并发会话、单轮工具步骤，以及会话生命周期内的累计步数。"
	>
		<SettingsField label="同时运行的会话" id="session-max-concurrent">
			<MaterialNumberField
				id="session-max-concurrent"
				value={session.max_concurrent}
				min={1}
				max={10}
				onChange={withNumberValue((value) => (session.max_concurrent = value))}
			/>
		</SettingsField>
		<SettingsField
			label="每轮最大步骤数"
			id="session-max-steps"
			description="一次运行（包括暂停后恢复）的 ReAct 步骤上限。"
		>
			<MaterialNumberField
				id="session-max-steps"
				value={session.max_steps}
				min={1}
				max={1000}
				onChange={withNumberValue((value) => (session.max_steps = value))}
			/>
		</SettingsField>
		<SettingsField
			label="会话累计步骤上限"
			id="session-lifetime-max-steps"
			description="跨暂停和恢复累计；设为 0 表示不限。"
		>
			<MaterialNumberField
				id="session-lifetime-max-steps"
				value={session.session_max_steps ?? 0}
				min={0}
				max={100000}
				onChange={withNumberValue((value) => (session.session_max_steps = value || null))}
			/>
		</SettingsField>
		<SettingsField
			label="会话保留天数"
			id="session-history-retention"
			description="按会话创建时间清理会话及其历史；设为 0 表示不自动清理。"
		>
			<MaterialNumberField
				id="session-history-retention"
				value={session.history_retention_days}
				min={0}
				max={365}
				onChange={withNumberValue((value) => (session.history_retention_days = value))}
			/>
		</SettingsField>
	</SettingsSection>

	<SettingsSection title="命令行工具" description="Shell 工具未指定解释器时使用这里的默认值。">
		<SettingsField label="默认 Shell" id="default-shell">
			<MaterialSelect
				id="default-shell"
				value={defaultShell}
				options={shellOptions()}
				onChange={withStringValue((value) =>
					onDefaultShellChange(value as ShellChoiceInput),
				)}
			/>
		</SettingsField>
		{#if defaultShell === 'pwsh' && shellAvailable.pwsh === false}
			<div class="shell-warning" role="status">
				<p>未检测到 PowerShell 7，命令将无法执行。请先安装：</p>
				<code>winget install Microsoft.PowerShell</code>
			</div>
		{/if}
	</SettingsSection>

	<SettingsSection title="记忆" description="设置对话中可检索的近期消息，并管理自动事实提取。">
		<SettingsField
			label="近期消息窗口"
			id="memory-window-size"
			description="每轮对话中优先保留的近期消息条数。"
		>
			<MaterialNumberField
				id="memory-window-size"
				value={memory.session_window_size}
				min={10}
				max={500}
				onChange={withNumberValue((value) => (memory.session_window_size = value))}
			/>
		</SettingsField>
		<SettingsField label="自动提取事实" description="从对话中整理可在后续会话中使用的事实。">
			<MaterialSwitch
				checked={memory.fact_inference_enabled}
				ariaLabel="自动提取事实"
				onChange={withBooleanValue((value) => (memory.fact_inference_enabled = value))}
			/>
		</SettingsField>
		<SettingsField label="记忆维护" description="清理重复、敏感、过期事实和残留向量。">
			<div class="maintenance-actions">
				<MaterialButton
					variant="outlined"
					label={memoryMaintenance.running ? '维护中…' : '立即维护'}
					onclick={() => onRunMaintenance()}
					disabled={memoryMaintenance.running}
				/>
				{#if memoryMaintenance.lastCount !== null}
					<span>上次清理 {memoryMaintenance.lastCount} 项</span>
				{/if}
			</div>
		</SettingsField>
	</SettingsSection>
</div>

<style>
	.settings-behavior {
		min-width: 0;
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
	.maintenance-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		flex-wrap: wrap;
	}
	.maintenance-actions span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
</style>
