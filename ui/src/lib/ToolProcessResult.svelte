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

	let processFilter = $state('');
	let processShowAll = $state(false);
	const processVisibleLimit = 50;
	let processList = $derived(/** @type {any[]} */ (Array.isArray(data.processes) ? data.processes : []));
	let filteredProcesses = $derived(
		processFilter
			? processList.filter((process) =>
					String(process.name ?? '')
						.toLowerCase()
						.includes(processFilter.toLowerCase()),
				)
			: processList,
	);
	let visibleProcesses = $derived(
		processFilter || processShowAll
			? filteredProcesses
			: filteredProcesses.slice(0, processVisibleLimit),
	);
	let maxProcMem = $derived(
		processList.reduce((max, process) => Math.max(max, Number(process.memory) || 0), 0),
	);
	/** @param {any} process */
	function memPct(process) {
		if (!maxProcMem) return 0;
		return Math.min(100, ((Number(process.memory) || 0) / maxProcMem) * 100);
	}
	/** @type {Record<string, string>} */
	const processStatusLabels = {
		Run: '运行中',
		Sleep: '休眠',
		Idle: '空闲',
		Stop: '已停止',
		Zombie: '僵尸',
		Dead: '已结束',
		Tracing: '跟踪',
		Unknown: '未知',
	};
	/** @param {any} status */
	function procStatusLabel(status) {
		return processStatusLabels[String(status ?? '')] ?? String(status ?? '未知');
	}
	/** @param {any} status */
	function procStatusClass(status) {
		const normalized = String(status ?? '').toLowerCase();
		if (normalized.includes('run')) return 'running';
		if (normalized.includes('sleep') || normalized.includes('idle')) return 'idle';
		if (normalized.includes('zombie') || normalized.includes('dead')) return 'failed';
		if (normalized.includes('stop') || normalized.includes('tracing')) return 'cancelled';
		return 'not_found';
	}
</script>

<div class="tool-card-count">
	{#if processFilter}{filteredProcesses.length} / {processList.length} 个进程{:else}{processList.length} 个进程{/if}
</div>
<input
	class="tool-search"
	type="search"
	placeholder="筛选进程..."
	bind:value={processFilter}
	aria-label="筛选进程"
/>
<div class="tool-card-list">
	<table class="proc-table">
		<thead>
			<tr><th>进程</th><th>PID</th><th>CPU</th><th>内存</th><th>状态</th></tr>
		</thead>
		<tbody>
			{#each visibleProcesses as process (process.pid)}
				<tr>
					<td class="proc-name" title={process.name}>{process.name}</td>
					<td class="proc-num">{process.pid}</td>
					<td class="proc-num proc-meter-cell">
						<span class="proc-meter"
							><span
								class="proc-meter-fill"
								style="width: {clampPct(process.cpu)}%"
							></span></span
						>{Number(process.cpu ?? 0).toFixed(1)}%
					</td>
					<td class="proc-num proc-meter-cell">
						<span class="proc-meter"
							><span
								class="proc-meter-fill"
								style="width: {memPct(process)}%"
							></span></span
						>{fmtBytes(process.memory)}
					</td>
					<td class="proc-status"
						><span
							class="status-badge status-{procStatusClass(process.status)}"
							>{procStatusLabel(process.status)}</span
						></td
					>
				</tr>
			{/each}
		</tbody>
	</table>
</div>
{#if !processFilter && processList.length > processVisibleLimit}
	<button
		class="show-all-btn"
		type="button"
		onclick={() => (processShowAll = !processShowAll)}
	>
		{processShowAll ? '收起' : `显示全部 ${processList.length} 个进程`}
	</button>
{/if}

<style>
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.tool-search {
		width: 100%;
		box-sizing: border-box;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-on-surface);
		font-size: 11px;
		padding: 4px var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-xs);
		outline: none;
	}
	.tool-search:focus {
		border-color: var(--md-sys-color-primary);
	}
	.proc-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 11px;
	}
	.proc-table th {
		position: sticky;
		top: 0;
		background: color-mix(
			in srgb,
			var(--md-sys-color-secondary-container) 45%,
			var(--md-sys-color-surface)
		);
		text-align: left;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		padding: 2px var(--md-sys-space-2xs);
		font-size: 10px;
	}
	.proc-table td {
		padding: 2px var(--md-sys-space-2xs);
		border-bottom: 1px solid color-mix(in srgb, var(--md-sys-color-on-surface) 6%, transparent);
	}
	.proc-name {
		max-width: 150px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface);
	}
	.proc-num {
		text-align: right;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
	}
	.proc-meter-cell {
		white-space: nowrap;
	}
	.proc-meter {
		display: inline-block;
		width: 36px;
		height: 4px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-highest);
		overflow: hidden;
		vertical-align: middle;
		margin-right: 4px;
	}
	.proc-meter-fill {
		display: block;
		height: 100%;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary);
	}
	.proc-status {
		padding-right: var(--md-sys-space-2xs) !important;
	}
	.status-badge {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		padding: 1px 8px;
		border-radius: var(--md-sys-shape-full);
	}
	.status-running {
		background: var(--md-sys-color-secondary);
		color: var(--md-sys-color-on-secondary);
	}
	.status-failed {
		background: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
	}
	.status-cancelled,
	.status-not_found,
	.status-idle {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.show-all-btn {
		width: 100%;
		box-sizing: border-box;
		margin-top: var(--md-sys-space-xs);
		background: transparent;
		border: 1px dashed var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-primary);
		font-size: 11px;
		font-weight: 600;
		padding: 4px;
		cursor: pointer;
	}
	.show-all-btn:hover {
		background: color-mix(in srgb, var(--md-sys-color-primary) 8%, transparent);
	}
</style>
