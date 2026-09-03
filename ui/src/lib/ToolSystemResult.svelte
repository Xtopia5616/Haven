<script>
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
	/** @param {string} text */
	async function copyEnvValue(text) {
		try {
			await navigator.clipboard.writeText(text);
		} catch {
			// Clipboard unavailable — ignore.
		}
	}
</script>

{#if data.os}
	<div class="sys-os">
		<span class="sys-os-name">{data.os.name || '系统'}</span>
		{#if data.os.hostname}<span class="sys-os-host">{data.os.hostname}</span>{/if}
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
						>{variable.value ?? '(未设置)'}</span
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
		<span class="env-value" title={data.value ?? ''}>{data.value ?? '(未设置)'}</span>
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
			>{data.battery_status ?? 'unknown'}{data.ac_power === 'online' ? ' · 已接电源' : ''}</span
		>
	</div>
{:else if data.locked || data.sleep || data.hibernate}
	<p class="tool-card-empty">
		{data.locked ? '已锁定' : data.sleep ? '已睡眠' : '已休眠'}
	</p>
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
	.env-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
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
