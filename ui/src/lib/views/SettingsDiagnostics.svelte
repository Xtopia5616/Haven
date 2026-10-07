<script lang="ts">
	import { tick } from 'svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import SettingsField from '$lib/SettingsField.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { readPerformanceMetricsSnapshot } from '$lib/performanceMetrics.ts';
	import { readLogInfo, readLogTail } from '$lib/diagnosticsCommands.ts';
	import { withBooleanValue, withStringValue } from '$lib/typedCallbacks.ts';
	import type { LogConfigInput } from '$lib/contracts/generatedCommands.ts';

	interface Props {
		log: Required<LogConfigInput>;
	}

	let { log }: Props = $props();
	let logView = $state<{ open: boolean; path: string; content: string; loading: boolean }>({
		open: false,
		path: '',
		content: '',
		loading: false,
	});
	let performanceMetricsLoading = $state(false);
	let logPreEl = $state<HTMLPreElement | null>(null);

	async function refreshLogs() {
		try {
			const data = await readLogTail({ maxLines: 300 });
			logView.path = data.path;
			logView.content = data.content;
		} catch (error) {
			reportError(error, {
				context: 'SettingsDiagnostics',
				message: '无法读取日志',
				log: false,
			});
		}
	}

	async function openLogViewer() {
		logView.loading = true;
		try {
			const info = await readLogInfo();
			if (!info?.enabled) {
				addNotification('文件日志未启用，请先打开文件日志', 'warning', 4000);
				return;
			}
			await refreshLogs();
			logView.open = true;
		} catch (error) {
			reportError(error, {
				context: 'SettingsDiagnostics',
				message: '无法读取日志',
				log: false,
			});
		} finally {
			logView.loading = false;
		}
	}

	async function exportPerformanceSnapshot() {
		performanceMetricsLoading = true;
		try {
			const snapshot = await readPerformanceMetricsSnapshot();
			const blob = new Blob([JSON.stringify(snapshot, null, 2)], {
				type: 'application/json',
			});
			const url = URL.createObjectURL(blob);
			const link = document.createElement('a');
			link.href = url;
			link.download = `haven-performance-metrics-${new Date().toISOString().replaceAll(':', '-')}.json`;
			document.body.appendChild(link);
			link.click();
			link.remove();
			URL.revokeObjectURL(url);
			addNotification('性能指标已导出', 'success');
		} catch (error) {
			reportError(error, {
				context: 'SettingsDiagnostics',
				message: '导出性能指标失败',
				log: false,
			});
		} finally {
			performanceMetricsLoading = false;
		}
	}

	$effect(() => {
		if (logView.open && logPreEl)
			void tick().then(() => {
				if (logPreEl) logPreEl.scrollTop = logPreEl.scrollHeight;
			});
	});
</script>

<div class="settings-diagnostics">
	<SettingsSection
		title="日志"
		description="日志配置由后端应用；路径留空时使用 Haven 默认日志目录。"
	>
		<SettingsField label="文件日志" description="将后端运行日志写入本机文件。">
			<MaterialSwitch
				checked={log.file_enabled}
				ariaLabel="启用文件日志"
				onChange={withBooleanValue((value) => (log.file_enabled = value))}
			/>
		</SettingsField>
		<SettingsField label="日志级别" id="log-level" description="级别越详细，记录的信息越多。">
			<MaterialSelect
				id="log-level"
				value={log.level}
				options={[
					{ value: 'trace', label: 'Trace · 最详细' },
					{ value: 'debug', label: 'Debug' },
					{ value: 'info', label: 'Info · 默认' },
					{ value: 'warn', label: 'Warn' },
					{ value: 'error', label: 'Error · 仅错误' },
				]}
				onChange={withStringValue(
					(value) => (log.level = value as NonNullable<LogConfigInput['level']>),
				)}
			/>
		</SettingsField>
		<SettingsField
			label="日志文件路径"
			description="此路径由配置文件管理，设置页保存时会保留当前值。"
		>
			<code class="log-path-value">{log.file_path || '默认位置（应用数据目录）'}</code>
		</SettingsField>
		<SettingsField label="最近日志" description="读取本机日志文件的最近 300 行。">
			<MaterialButton
				variant="outlined"
				label={logView.loading ? '读取中…' : '查看日志'}
				onclick={openLogViewer}
				disabled={logView.loading || !log.file_enabled}
			/>
		</SettingsField>
	</SettingsSection>

	<SettingsSection
		title="性能诊断"
		description="导出当前进程的计数与采样数据，便于定位性能问题。"
	>
		<SettingsField label="性能指标">
			<MaterialButton
				variant="outlined"
				label={performanceMetricsLoading ? '导出中…' : '导出性能指标'}
				onclick={exportPerformanceSnapshot}
				disabled={performanceMetricsLoading}
			/>
		</SettingsField>
	</SettingsSection>
</div>

{#if logView.open}
	<MaterialDialog
		open={true}
		title="日志查看"
		dialogClass="md-dialog--wide"
		onClose={() => (logView.open = false)}
	>
		{#snippet children()}
			{#if logView.path}<p class="log-path" title={logView.path}>{logView.path}</p>{/if}
			<pre class="log-viewer" bind:this={logPreEl}>{logView.content ||
					'（暂无日志内容）'}</pre>
		{/snippet}
		{#snippet footer()}
			<MaterialButton
				variant="outlined"
				label="刷新"
				onclick={refreshLogs}
				disabled={logView.loading}
			/>
			<MaterialButton variant="text" label="关闭" onclick={() => (logView.open = false)} />
		{/snippet}
	</MaterialDialog>
{/if}

<style>
	.settings-diagnostics {
		min-width: 0;
	}
	.log-path-value {
		display: block;
		max-width: var(--md-comp-settings-control-width);
		overflow-wrap: anywhere;
		color: var(--md-sys-color-on-surface-variant);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	:global(.md-dialog--wide) {
		width: min(760px, 92vw);
	}
	.log-path {
		margin: 0 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		word-break: break-all;
	}
	.log-viewer {
		box-sizing: border-box;
		max-height: 60vh;
		overflow: auto;
		margin: 0;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: pre;
	}
</style>
