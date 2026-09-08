<script>
	import JsonView from '$lib/JsonView.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolCardList from '$lib/ToolCardList.svelte';
	import ToolSearch from '$lib/ToolSearch.svelte';

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
	function procStatusTone(status) {
		const normalized = String(status ?? '').toLowerCase();
		if (normalized.includes('run')) return 'success';
		if (normalized.includes('zombie') || normalized.includes('dead')) return 'error';
		if (normalized.includes('sleep') || normalized.includes('idle')) return 'neutral';
		if (normalized.includes('stop') || normalized.includes('tracing')) return 'warning';
		return 'neutral';
	}
</script>

{#if Array.isArray(data.processes)}
	<div class="tool-card-count">
		{#if processFilter}{filteredProcesses.length} / {processList.length} 个进程{:else}{processList.length} 个进程{/if}
	</div>
	<ToolSearch
		value={processFilter}
		onInput={(/** @type {string} */ value) => (processFilter = value)}
		placeholder="筛选进程..."
		ariaLabel="筛选进程"
	/>
	<ToolCardList>
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
						<td class="proc-status">
							<StatusBadge label={procStatusLabel(process.status)} tone={procStatusTone(process.status)} />
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
		{#if visibleProcesses.length === 0}<p class="tool-card-empty">没有匹配的进程</p>{/if}
	</ToolCardList>
	{#if !processFilter && processList.length > processVisibleLimit}
		<MaterialButton
			variant="text"
			className="show-all-btn"
			label={processShowAll ? '收起' : `显示全部 ${processList.length} 个进程`}
			onclick={() => (processShowAll = !processShowAll)}
		/>
	{/if}
{:else if data.operation === 'kill' && data.killed != null}
	<div class="process-action-row">
		<StatusBadge label="已终止" tone="success" />
		<span class="process-action-id">PID {data.killed}</span>
	</div>
{:else if data.operation}
	<div class="tool-card-meta">进程操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{:else}
	<p class="tool-card-empty">没有进程结果</p>
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.proc-table {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
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
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
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
	:global(.md-btn.show-all-btn) {
		width: 100%;
		box-sizing: border-box;
		margin-top: var(--md-sys-space-xs);
		background: transparent;
		border: 1px dashed var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		color: var(--md-sys-color-primary);
		min-height: var(--md-comp-button-small-height);
	}
	:global(.md-btn.show-all-btn:hover) {
		background: color-mix(in srgb, var(--md-sys-color-primary) 8%, transparent);
	}
	.tool-card-empty {
		margin: var(--md-sys-space-sm) 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.process-action-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.process-action-id {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		color: var(--md-sys-color-on-surface);
	}
</style>
