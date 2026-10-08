<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import StatusBadge from '$lib/StatusBadge.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import ToolSearch from '$lib/ToolSearch.svelte';
	import { clampPercentage, formatByteSize } from '$lib/toolResultFormatting.ts';
	import type { ToolProcessOperation, ToolProcessStatus } from './toolResultPresentation.ts';

	interface ProcessEntry {
		name?: string;
		pid?: string | number;
		cpu?: number;
		memory?: number;
		status?: ToolProcessStatus | null;
	}

	interface Props {
		data?: {
			processes?: ProcessEntry[];
			operation?: ToolProcessOperation | null;
			killed?: string | number;
		};
	}

	let { data = {} }: Props = $props();

	let processFilter = $state('');
	let processList: ProcessEntry[] = $derived(Array.isArray(data.processes) ? data.processes : []);
	let filteredProcesses = $derived(
		processFilter
			? processList.filter((process) =>
					String(process.name ?? '')
						.toLowerCase()
						.includes(processFilter.toLowerCase()),
				)
			: processList,
	);
	let maxProcMem = $derived(
		processList.reduce((max, process) => Math.max(max, Number(process.memory) || 0), 0),
	);
	function memPct(process: ProcessEntry) {
		if (!maxProcMem) return 0;
		return Math.min(100, ((Number(process.memory) || 0) / maxProcMem) * 100);
	}
	const processStatusLabels: Partial<Record<ToolProcessStatus, string>> = {
		Run: '运行中',
		Sleep: '休眠',
		Idle: '空闲',
		Stop: '已停止',
		Zombie: '僵尸',
		Dead: '已结束',
		Tracing: '跟踪',
		Unknown: '未知',
		Wakekill: '唤醒终止',
		Waking: '唤醒中',
		Parked: '已挂起',
		LockBlocked: '锁等待',
		UninterruptibleDiskSleep: '不可中断等待',
		Suspended: '已暂停',
	};
	function procStatusLabel(status: ToolProcessStatus | null | undefined) {
		return (status && processStatusLabels[status]) || status || '未知';
	}
	function procStatusTone(status: ToolProcessStatus | null | undefined) {
		const normalized = status?.toLowerCase() ?? '';
		if (normalized.includes('run')) return 'success';
		if (normalized.includes('zombie') || normalized.includes('dead')) return 'error';
		if (normalized.includes('sleep') || normalized.includes('idle')) return 'neutral';
		if (normalized.includes('stop') || normalized.includes('tracing')) return 'warning';
		return 'neutral';
	}
</script>

{#if Array.isArray(data.processes)}
	<div class="tool-result-label">
		{#if processFilter}{filteredProcesses.length} / {processList.length} 个进程{:else}{processList.length}
			个进程{/if}
	</div>
	<ToolSearch
		value={processFilter}
		onInput={(/** @type {string} */ value) => (processFilter = value)}
		placeholder="筛选进程..."
		ariaLabel="筛选进程"
	/>
	<ToolResultList items={filteredProcesses}>
		{#snippet children(visibleProcesses)}
			<div class="tool-result-scroll-area">
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
											style="width: {clampPercentage(process.cpu)}%"
										></span></span
									>{Number(process.cpu ?? 0).toFixed(1)}%
								</td>
								<td class="proc-num proc-meter-cell">
									<span class="proc-meter"
										><span
											class="proc-meter-fill"
											style="width: {memPct(process)}%"
										></span></span
									>{formatByteSize(process.memory)}
								</td>
								<td class="proc-status">
									<StatusBadge
										label={procStatusLabel(process.status)}
										tone={procStatusTone(process.status)}
									/>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
				{#if visibleProcesses.length === 0}<p
						class="tool-result-message tool-result-message--spaced"
					>
						没有匹配的进程
					</p>{/if}
			</div>
		{/snippet}
	</ToolResultList>
{:else if data.operation === 'kill' && data.killed != null}
	<div class="process-action-row">
		<StatusBadge label="已终止" tone="success" />
		<span class="process-action-id">PID {data.killed}</span>
	</div>
{:else if data.operation}
	<div class="tool-result-meta">进程操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{:else}
	<p class="tool-result-message tool-result-message--spaced">没有进程结果</p>
{/if}

<style>
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
