<script>
	import logger from './logger.ts';
	import { formatError } from './formatError.ts';
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();

	/** @param {unknown} value */
	function clampPct(value) {
		const n = Number(value);
		if (!Number.isFinite(n)) return 0;
		return Math.max(0, Math.min(100, n));
	}

	/** @param {unknown} value */
	function fmtBytes(value) {
		const n = Number(value);
		if (!Number.isFinite(n) || n < 0) return '—';
		if (n < 1024) return `${n} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let unit = n;
		let index = -1;
		while (unit >= 1024 && index < units.length - 1) {
			unit /= 1024;
			index++;
		}
		return `${unit >= 100 ? unit.toFixed(0) : unit.toFixed(1)} ${units[index]}`;
	}

	/** @param {unknown} value */
	function fmtUptime(value) {
		const secs = Number(value);
		if (!Number.isFinite(secs) || secs < 0) return null;
		const days = Math.floor(secs / 86400);
		const hours = Math.floor((secs % 86400) / 3600);
		const minutes = Math.floor((secs % 3600) / 60);
		if (days > 0) return `${days} 天 ${hours} 小时`;
		if (hours > 0) return `${hours} 小时 ${minutes} 分`;
		return `${minutes} 分钟`;
	}

	/** @param {unknown} value */
	function networkStateLabel(value) {
		return (
			{
				up: '已连接',
				down: '已断开',
				dormant: '待机',
				unknown: '未知',
			}[String(value ?? '').toLowerCase()] ?? String(value ?? '未知')
		);
	}

	/** @param {unknown} value */
	function batteryStatusLabel(value) {
		return (
			{
				high: '电量充足',
				low: '电量较低',
				critical: '电量严重不足',
				charging: '充电中',
				unknown: '状态未知',
			}[String(value ?? '').toLowerCase()] ?? String(value ?? '状态未知')
		);
	}

	let envFilter = $state('');
	let envList = $derived(Array.isArray(data.variables) ? data.variables : []);
	let filteredEnv = $derived(
		envFilter
			? envList.filter((variable) => {
					const query = envFilter.toLowerCase();
					return (
						String(variable.name ?? '')
							.toLowerCase()
							.includes(query) ||
						String(variable.value ?? '')
							.toLowerCase()
							.includes(query)
					);
				})
			: envList,
	);
	let hasStructuredView = $derived(
		!!data.os ||
		!!data.cpu ||
		!!data.memory ||
		!!data.disks ||
		Array.isArray(data.displays) ||
		Array.isArray(data.variables) ||
		Array.isArray(data.networks) ||
		!!data.network_summary ||
		!!data.user ||
		!!data.locale ||
		Array.isArray(data.values) ||
		Array.isArray(data.subkeys) ||
		!!data.name ||
		data.battery_percent != null ||
		data.ac_power ||
		data.battery_present != null ||
		data.battery_saver ||
		data.locked ||
		data.sleep ||
		data.hibernate ||
		data.available === false,
	);
	/** @param {string} text */
	async function copyEnvValue(text) {
		try {
			await navigator.clipboard.writeText(text);
		} catch (error) {
			logger.warn('ToolSystemResult', 'environment value copy failed', formatError(error));
		}
	}
</script>

{#if data.os}
	<div class="sys-os">
		<span class="sys-os-name">{data.os.name || '系统'}</span>
		{#if data.os.hostname}<span class="sys-os-host">{data.os.hostname}</span>{/if}
	</div>
{/if}
{#if data.user}
	<div class="tool-card-count">用户信息</div>
	<div class="info-grid">
		{#if data.user.username}<div class="info-label">用户</div><div class="info-value">{data.user.username}</div>{/if}
		{#if data.user.computer_name}<div class="info-label">计算机</div><div class="info-value">{data.user.computer_name}</div>{/if}
		{#if data.user.home}<div class="info-label">主目录</div><div class="info-value" title={data.user.home}>{data.user.home}</div>{/if}
		{#if data.user.cwd}<div class="info-label">当前目录</div><div class="info-value" title={data.user.cwd}>{data.user.cwd}</div>{/if}
	</div>
{/if}
{#if data.locale}
	<div class="tool-card-count">时间与区域</div>
	<div class="info-grid">
		{#if data.locale.locale_name}<div class="info-label">区域</div><div class="info-value">{data.locale.locale_name}</div>{/if}
		{#if data.locale.ui_language}<div class="info-label">界面语言</div><div class="info-value">{data.locale.ui_language}</div>{/if}
		{#if data.locale.local_time}<div class="info-label">本地时间</div><div class="info-value">{data.locale.local_time}</div>{/if}
		{#if data.locale.timezone_offset_hours != null}<div class="info-label">时区</div><div class="info-value">UTC{data.locale.timezone_offset_hours >= 0 ? '+' : ''}{data.locale.timezone_offset_hours}</div>{/if}
	</div>
{/if}
{#if Array.isArray(data.networks)}
	<div class="tool-card-count">{data.count ?? data.networks.length} 个网络接口</div>
	<div class="tool-card-list">
		{#each data.networks as network (network.name)}
			<div class="network-row">
				<div class="network-main">
					<span class="network-name">{network.name || '未命名接口'}</span>
					<span class="network-state">{networkStateLabel(network.state)}</span>
				</div>
				{#if Array.isArray(network.ips) && network.ips.length > 0}<div class="network-ips">{network.ips.join(' · ')}</div>{/if}
			</div>
		{/each}
	</div>
{/if}
{#if data.network_summary}
	<div class="tool-card-count">网络概况</div>
	<div class="info-grid">
		<div class="info-label">接口</div><div class="info-value">{data.network_summary.interface_count ?? 0}</div>
		<div class="info-label">可用或未知</div><div class="info-value">{data.network_summary.up_or_unknown ?? 0}</div>
		<div class="info-label">已断开</div><div class="info-value">{data.network_summary.down ?? 0}</div>
	</div>
{/if}
{#if Array.isArray(data.values) || Array.isArray(data.subkeys)}
	<div class="tool-card-count">注册表{data.path ? ` · ${data.path}` : ''}</div>
	{#if Array.isArray(data.values) && data.values.length > 0}
		<div class="tool-card-list">
			{#each data.values as value (value)}<div class="env-row"><span class="env-name">{value}</span></div>{/each}
		</div>
	{:else if Array.isArray(data.subkeys) && data.subkeys.length > 0}
		<div class="tool-card-list">
			{#each data.subkeys as key (key)}<div class="env-row"><span class="env-name">{key}</span></div>{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有注册表值或子项</p>
	{/if}
{/if}
{#if data.deleted}
	<div class="system-action-row">
		<span class="system-action">已删除</span>
		{#if data.path}<span class="info-value" title={data.path}>{data.path}</span>{/if}
	</div>
{/if}
{#if data.available === false}
	<p class="tool-card-empty">{data.note || data.reason || '此系统能力当前不可用'}</p>
{/if}
{#if data.ac_power || data.battery_present != null || data.battery_saver}
	<div class="info-grid power-info">
		{#if data.ac_power}<div class="info-label">电源</div><div class="info-value">{data.ac_power === 'online' ? '接通电源' : data.ac_power === 'offline' ? '使用电池' : data.ac_power}</div>{/if}
		{#if data.battery_present === false}<div class="info-label">电池</div><div class="info-value">未检测到电池</div>{/if}
		{#if data.battery_saver}<div class="info-label">省电模式</div><div class="info-value">已开启</div>{/if}
	</div>
{/if}
{#if data.cpu}
	<div class="meter-row">
		<span class="meter-label">CPU</span>
		<span class="meter-value">{Number(data.cpu.usage_pct ?? 0).toFixed(1)}%</span>
		<span class="meter-track"
			><span class="meter-fill" style="width: {clampPct(data.cpu.usage_pct)}%"></span></span
		>
		<span class="meter-sub">{data.cpu.cores ?? 0} 核 / {data.cpu.logical_cpus ?? 0} 线程</span>
	</div>
{/if}
{#if data.memory}
	<div class="meter-row">
		<span class="meter-label">内存</span>
		<span class="meter-value"
			>{fmtBytes(data.memory.used_bytes)} / {fmtBytes(data.memory.total_bytes)}</span
		>
		<span class="meter-track"
			><span
				class="meter-fill"
				style="width: {clampPct(
					(Number(data.memory.used_bytes) / Math.max(Number(data.memory.total_bytes), 1)) * 100,
				)}%"
			></span></span
		>
	</div>
{/if}
{#if Array.isArray(data.disks)}
	{#each data.disks as disk (disk.mount)}
		<div class="meter-row">
			<span class="meter-label">{disk.mount}</span>
			<span class="meter-value"
				>{fmtBytes(Number(disk.total_bytes) - Number(disk.available_bytes))} /
				{fmtBytes(disk.total_bytes)}</span
			>
			<span class="meter-track"
				><span
					class="meter-fill"
					style="width: {clampPct(
						(1 - Number(disk.available_bytes) / Math.max(Number(disk.total_bytes), 1)) * 100,
					)}%"
				></span></span
			>
		</div>
	{/each}
{/if}
{#if data.os?.uptime_secs != null}
	<div class="tool-card-meta">运行时长 {fmtUptime(data.os.uptime_secs)}</div>
{/if}
{#if Array.isArray(data.displays)}
	<div class="tool-card-count">{data.displays.length} 个显示器</div>
	<div class="tool-card-list">
		{#each data.displays as display (display.name ?? display.left)}
			<div class="window-row">
				<span class="window-title">{display.name || 'Display'}{display.primary ? ' · 主屏' : ''}</span>
				<span class="window-pid">{display.width}×{display.height}</span>
			</div>
		{/each}
	</div>
{/if}
{#if Array.isArray(data.variables)}
	<div class="tool-card-count">
		{#if envFilter}{filteredEnv.length} / {envList.length} 个变量{:else}{envList.length} 个变量{/if}
	</div>
	<input
		class="tool-search"
		type="search"
		placeholder="筛选变量..."
		bind:value={envFilter}
		aria-label="筛选变量"
	/>
	{#if filteredEnv.length > 0}
		<div class="tool-card-list">
			{#each filteredEnv as variable (variable.name)}
				<div class="env-row">
					<span class="env-name" title={variable.name}>{variable.name}</span>
					<span class="env-value" title={variable.value ?? ''}
						>{variable.value != null ? variable.value : '仅名称（未读取）'}</span
					>
					{#if typeof variable.value === 'string' && variable.value}
						<button
							class="env-copy"
							type="button"
							aria-label="复制值"
							title="复制值"
							onclick={() => copyEnvValue(variable.value)}>⧉</button
						>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">没有匹配的变量</p>
	{/if}
{:else if data.name && ('value' in data || data.set || data.removed)}
	<div class="env-row">
		<span class="env-name">{data.name}</span>
		<span class="env-value" title={data.value ?? ''}>{data.value != null ? data.value : data.removed ? '已移除' : 'value' in data ? '未设置' : '仅名称（未读取）'}</span>
	</div>
{/if}
{#if data.battery_percent != null}
	<div class="meter-row">
		<span class="meter-label">电池</span>
		<span class="meter-value">{data.battery_percent}%</span>
		<span class="meter-track"
			><span class="meter-fill" style="width: {clampPct(data.battery_percent)}%"></span></span
		>
		<span class="meter-sub"
			>{batteryStatusLabel(data.battery_status)}{data.ac_power === 'online' ? ' · 已接电源' : ''}</span
		>
	</div>
{:else if data.locked || data.sleep || data.hibernate}
	<p class="tool-card-empty">
		{data.locked ? '已锁定' : data.sleep ? '已睡眠' : '已休眠'}
	</p>
{/if}
{#if !hasStructuredView}
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.sys-os {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.sys-os-name {
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.sys-os-host {
		font-size: var(--md-sys-typescale-code-size);
		font-family: var(--md-sys-typescale-mono);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.meter-row {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		column-gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-xs);
	}
	.meter-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.meter-value {
		grid-column: 2;
		font-size: var(--md-sys-typescale-code-size);
		font-family: var(--md-sys-typescale-mono);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.meter-track {
		grid-column: 1 / -1;
		height: 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-high);
		overflow: hidden;
	}
	.meter-fill {
		display: block;
		height: 100%;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary);
		transition: width 0.4s ease;
	}
	.meter-sub {
		grid-column: 2;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.window-row,
	.env-row,
	.network-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.info-grid {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: var(--md-sys-space-xs) var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
	}
	.info-label,
	.info-value {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.info-label {
		color: var(--md-sys-color-on-surface-variant);
	}
	.info-value {
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.network-row {
		display: block;
	}
	.network-main {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-xs);
	}
	.network-name {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.network-state,
	.network-ips {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.network-state {
		flex: none;
	}
	.network-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.network-ips {
		margin-top: var(--md-sys-space-2xs);
		font-family: var(--md-sys-typescale-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.window-row:nth-child(odd),
	.env-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.window-title,
	.env-name,
	.env-value {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.window-title {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface);
	}
	.window-pid {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.env-name {
		flex: none;
		font-weight: 600;
		color: var(--md-sys-color-secondary);
	}
	.env-value {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-search {
		width: 100%;
		box-sizing: border-box;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 4px var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-xs);
		outline: none;
	}
	.tool-search:focus {
		border-color: var(--md-sys-color-primary);
	}
	.system-action-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.system-action {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.env-copy {
		flex: none;
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		padding: 2px 4px;
		border-radius: 4px;
		cursor: pointer;
		opacity: 0;
		transition:
			opacity 0.15s ease,
			background-color 0.15s ease,
			color 0.15s ease;
	}
	.env-row:hover .env-copy,
	.env-copy:focus-visible {
		opacity: 1;
	}
	.env-copy:hover {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}
</style>
