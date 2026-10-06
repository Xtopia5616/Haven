<script lang="ts">
	import { onDestroy } from 'svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { themeStore } from '$lib/themeStore.ts';
	import { withBooleanValue } from '$lib/typedCallbacks.ts';
	import type { NotificationConfigInput } from '$lib/contracts/generatedCommands.ts';

	interface Props {
		notification: Required<NotificationConfigInput>;
		autostartEnabled: boolean;
		onAutostartChange?: (value: boolean) => void;
	}

	let { notification, autostartEnabled, onAutostartChange = () => {} }: Props = $props();

	type NotificationKey = keyof Required<NotificationConfigInput>;
	const NOTIFICATION_EVENTS: Array<{ key: NotificationKey; label: string }> = [
		{ key: 'session_created', label: '会话开始' },
		{ key: 'session_completed', label: '会话完成' },
		{ key: 'session_paused', label: '会话暂停' },
		{ key: 'session_resumed', label: '会话恢复' },
		{ key: 'session_error', label: '会话出错' },
		{ key: 'permission_requested', label: '权限请求' },
		{ key: 'tool_run_completed', label: '任务完成' },
	];

	let currentTheme = $state(themeStore.currentTheme);
	let accent = $state(themeStore.currentAccent);
	let customAccentHex = $state(themeStore.isPreset ? '#2C5090' : themeStore.accentColor);
	const unsubscribeTheme = themeStore.subscribe((value) => {
		currentTheme = value.theme;
		accent = value.accent;
		customAccentHex = themeStore.isPreset ? customAccentHex : themeStore.accentColor;
	});
	onDestroy(unsubscribeTheme);

	function contrastText(hex: string) {
		const red = Number.parseInt(hex.slice(1, 3), 16);
		const green = Number.parseInt(hex.slice(3, 5), 16);
		const blue = Number.parseInt(hex.slice(5, 7), 16);
		const luminance = (0.299 * red + 0.587 * green + 0.114 * blue) / 255;
		return luminance > 0.5 ? '#000000' : '#ffffff';
	}
</script>

<div class="settings-appearance">
	<SettingsSection title="外观" description="主题和强调色会立即应用并保存在本机。">
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
			<div class="accent-picker" role="radiogroup" aria-label="强调色">
				{#each Object.entries(themeStore.presets) as [key, preset] (key)}
					<MaterialButton
						variant="filled"
						label={preset.label}
						className={`accent-swatch ${accent === key ? 'accent-swatch-selected' : ''}`}
						style={`background: ${preset.hex}; color: ${contrastText(preset.hex)}; --_btn-state: ${contrastText(preset.hex)}; border: 2px solid transparent; border-color: ${accent === key ? contrastText(preset.hex) : 'transparent'}`}
						role="radio"
						ariaChecked={accent === key}
						ariaLabel={`${preset.label} ${preset.hex}`}
						onclick={() => themeStore.setAccent(key)}
					/>
				{/each}
				<label
					class="accent-custom"
					class:accent-custom-selected={!themeStore.isPreset}
					for="custom-accent"
				>
					<span
						class="accent-custom-preview"
						style={`background-color: ${customAccentHex};`}
						aria-hidden="true"
					></span>
					<span class="accent-custom-label">自定义</span>
					<input
						id="custom-accent"
						type="text"
						class="custom-hex-input"
						placeholder="#RRGGBB"
						maxlength="7"
						value={customAccentHex}
						autocomplete="off"
						aria-label="自定义强调色十六进制值"
						oninput={(event) => {
							const value = (event.currentTarget as HTMLInputElement).value;
							customAccentHex = value;
							if (/^#[0-9a-f]{6}$/i.test(value)) themeStore.setAccent(value);
						}}
					/>
				</label>
			</div>
		</SettingsField>
	</SettingsSection>

	<SettingsSection title="通知" description="分别选择在 Haven 内和 Windows 桌面显示哪些事件。">
		<div class="notify-grid-header" aria-hidden="true">
			<span></span><span>应用内</span><span>Windows</span>
		</div>
		{#each NOTIFICATION_EVENTS as event (event.key)}
			<div class="notify-grid-row">
				<span class="notification-label">{event.label}</span>
				<MaterialSwitch
					checked={notification[event.key].in_app ?? true}
					ariaLabel={`${event.label}应用内提示`}
					onChange={withBooleanValue((value) => (notification[event.key].in_app = value))}
				/>
				<MaterialSwitch
					checked={notification[event.key].windows ?? false}
					ariaLabel={`${event.label} Windows 通知`}
					onChange={withBooleanValue(
						(value) => (notification[event.key].windows = value),
					)}
				/>
			</div>
		{/each}
		<p class="notification-note">
			Agent 通过 notify 工具发送的通知始终开启（应用内与 Windows），不受这些选项控制。
		</p>
	</SettingsSection>

	<SettingsSection title="Windows 启动" description="此选项会在保存设置时写入 Windows 启动项。">
		<SettingsField label="登录 Windows 时启动 Haven">
			<MaterialSwitch
				checked={autostartEnabled}
				ariaLabel="登录 Windows 时启动 Haven"
				onChange={withBooleanValue((value) => onAutostartChange(value))}
			/>
		</SettingsField>
	</SettingsSection>
</div>

<style>
	.settings-appearance {
		min-width: 0;
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
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		height: var(--md-comp-button-small-height);
		min-height: var(--md-comp-button-small-height);
		padding: 0 var(--md-sys-space-sm) 0 var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-comp-button-radius);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-small-size);
		cursor: text;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.accent-custom:hover {
		border-color: var(--md-sys-color-on-surface);
		background: var(--md-sys-color-surface-container);
	}
	.accent-custom:focus-within,
	.accent-custom.accent-custom-selected {
		border-color: var(--md-sys-color-primary);
		box-shadow: inset 0 0 0 1px var(--md-sys-color-primary);
	}
	.accent-custom-preview {
		width: 28px;
		height: 28px;
		flex: 0 0 28px;
		border-radius: var(--md-sys-shape-small);
		box-shadow: inset 0 0 0 1px
			color-mix(in srgb, var(--md-sys-color-on-surface) 18%, transparent);
	}
	.accent-custom-label {
		font-weight: 600;
		white-space: nowrap;
	}
	.custom-hex-input {
		width: 76px;
		height: 28px;
		box-sizing: border-box;
		padding: 0 var(--md-sys-space-xs);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		outline: none;
		color: var(--md-sys-color-on-surface);
		text-align: center;
	}
	.custom-hex-input:focus {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-focus-ring);
	}
	.custom-hex-input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
	.notify-grid-header,
	.notify-grid-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) var(--md-comp-switch-width) var(
				--md-comp-switch-width
			);
		gap: var(--md-sys-space-md);
		align-items: center;
	}
	.notify-grid-header {
		padding-bottom: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		margin-bottom: var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
	}
	.notify-grid-header span:not(:first-child) {
		text-align: center;
	}
	.notify-grid-row {
		min-height: var(--md-comp-list-item-one-line-height);
		padding-block: var(--md-sys-space-2xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.notify-grid-row:last-of-type {
		border-bottom: 0;
	}
	.notification-label,
	.notification-note {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.notification-note {
		margin: var(--md-sys-space-md) 0 0;
	}
</style>
