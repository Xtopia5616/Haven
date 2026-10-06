<script lang="ts">
	import MaterialIconButton from './MaterialIconButton.svelte';
	import StatusDot from './StatusDot.svelte';
	import { toolRunKindLabel } from '$lib/toolRunTerminology.ts';

	interface Props {
		overlay?: { isRecording?: boolean; processing?: boolean };
		executionPhase?: string;
		busySessions?: ReadonlySet<unknown>;
		conversationStatus?: string;
		runtime?: string;
		bootstrapReady?: boolean;
		llmConnected?: string | null;
		llmConnectionDetail?: string | null;
		awaitingBackgroundActive?: boolean;
		runningBackgroundToolRunCount?: number;
		pendingScheduledToolRuns?: unknown[];
		onOpenTasks?: () => void;
	}

	let {
		overlay = {},
		executionPhase = 'idle',
		busySessions = new Set(),
		conversationStatus = '空闲',
		runtime = 'tauri',
		bootstrapReady = true,
		llmConnected = null,
		llmConnectionDetail = null,
		awaitingBackgroundActive = false,
		runningBackgroundToolRunCount = 0,
		pendingScheduledToolRuns = [],
		onOpenTasks = () => {},
	}: Props = $props();

	const executionStatusLabel = $derived.by(() => {
		if (runtime === 'browser') return '浏览器预览';
		if (overlay.isRecording) return '录音中';
		if (overlay.processing) return '转写中';
		// Waiting/terminal states belong to the selected conversation. A running
		// session is more useful when shown at its current ReAct phase below.
		if (
			conversationStatus !== '空闲' &&
			conversationStatus !== '运行中' &&
			conversationStatus !== '排队中' &&
			busySessions.size <= 1
		)
			return conversationStatus;
		if (conversationStatus === '排队中' && busySessions.size <= 1) return '排队中';
		if (executionPhase === 'requesting') return '请求中';
		if (executionPhase === 'generating') return '生成中';
		if (executionPhase === 'waiting_result') return '等待结果';
		if (executionPhase === 'waiting_response') return '等待响应';
		if (executionPhase === 'queued') return '排队中';
		if (busySessions.size > 1) return `${busySessions.size} 个会话运行中`;
		if (conversationStatus === '运行中' || busySessions.size > 0) return '运行中';
		if (awaitingBackgroundActive) return '等待任务';
		if (runningBackgroundToolRunCount > 0) return toolRunKindLabel('background');
		if (!bootstrapReady) return '加载中';
		return '空闲';
	});
	const modelStatusLabel = $derived.by(() => {
		if (llmConnected === 'unconfigured') return '模型未配置';
		if (llmConnected === 'disconnected') return '模型不可用';
		if (llmConnected === null) return '检测中';
		return null;
	});
	const modelStatusTitle = $derived.by(() => {
		if (llmConnected === 'disconnected') {
			return `默认模型不可用：${llmConnectionDetail || '暂时无法确定具体原因'}。请到模型设置检查 API 地址、API Key 和代理`;
		}
		if (llmConnected === 'unconfigured') {
			return '默认模型未配置，请到模型设置填写 Provider、模型和 API Key';
		}
		return '正在检测默认模型连接';
	});
	const statusLabel = $derived.by(() => {
		if (runtime === 'browser' || overlay.isRecording || overlay.processing) {
			return executionStatusLabel;
		}
		// Persistent failures outrank routine loop progress. The tooltip keeps
		// the execution context visible while the single chip shows the priority.
		if (executionStatusLabel === '错误') return executionStatusLabel;
		if (modelStatusLabel === '模型不可用') return modelStatusLabel;
		if (['已暂停', '等待操作', '等待任务', '等待定时任务'].includes(executionStatusLabel)) {
			return executionStatusLabel;
		}
		if (modelStatusLabel === '模型未配置') return modelStatusLabel;
		if (executionStatusLabel !== '空闲' && executionStatusLabel !== '加载中') {
			return executionStatusLabel;
		}
		if (modelStatusLabel === '检测中') return modelStatusLabel;
		return executionStatusLabel;
	});

	const taskCount = $derived(runningBackgroundToolRunCount + pendingScheduledToolRuns.length);
	const hasTaskActivity = $derived(taskCount > 0 || awaitingBackgroundActive);

	const statusColor = $derived.by(() => {
		if (runtime === 'browser') return 'neutral';
		if (['录音中', '错误', '模型不可用'].includes(statusLabel)) return 'error';
		if (['生成中', '转写中'].includes(statusLabel)) return 'info';
		if (['等待结果', '等待任务', '等待定时任务', '后台任务'].includes(statusLabel))
			return 'tool';
		if (['已暂停', '等待操作', '模型未配置'].includes(statusLabel)) return 'warning';
		if (
			['检测中', '排队中', '请求中', '等待响应', '运行中', '加载中'].includes(statusLabel) ||
			busySessions.size > 0
		)
			return 'warning';
		return 'neutral';
	});

	const statusAnimating = $derived(
		[
			'录音中',
			'转写中',
			'生成中',
			'等待结果',
			'请求中',
			'等待响应',
			'排队中',
			'运行中',
			'后台任务',
			'加载中',
			'检测中',
		].includes(statusLabel),
	);

	const statusTitle = $derived.by(() => {
		if (runtime === 'browser') {
			return '当前是浏览器预览，Rust/Tauri 后端未启动；运行 cargo tauri dev 启动桌面应用';
		}
		const parts = [];
		if (executionStatusLabel !== statusLabel) {
			parts.push(`执行状态：${executionStatusLabel}`);
		}
		if (modelStatusLabel && modelStatusLabel !== statusLabel) {
			parts.push(`模型状态：${modelStatusLabel}（${modelStatusTitle}）`);
		} else if (modelStatusLabel === statusLabel) {
			parts.push(modelStatusTitle);
		}
		if (runningBackgroundToolRunCount > 0) {
			parts.push(`${runningBackgroundToolRunCount} 个${toolRunKindLabel('background')}运行中`);
		}
		if (pendingScheduledToolRuns.length > 0) {
			parts.push(`${pendingScheduledToolRuns.length} 条${toolRunKindLabel('scheduled')}`);
		}
		return parts.length > 0
			? `状态：${statusLabel}；${parts.join('；')}`
			: `状态：${statusLabel}`;
	});

	const toolRunTitle = $derived.by(() => {
		const parts = [];
		if (runningBackgroundToolRunCount > 0) parts.push(`${runningBackgroundToolRunCount} 个后台任务运行中`);
		if (pendingScheduledToolRuns.length > 0) {
			parts.push(`${pendingScheduledToolRuns.length} 条${toolRunKindLabel('scheduled')}`);
		}
		return parts.length > 0 ? `打开任务：${parts.join('，')}` : '打开任务查看详情';
	});
</script>

<div class="status-switch">
	<div class="status-chip" role="status" aria-label={`状态：${statusLabel}`} title={statusTitle}>
		<StatusDot color={statusColor} animate={statusAnimating} />
		<span class:recording-text={overlay.isRecording} class="status-text">{statusLabel}</span>
	</div>
	{#if hasTaskActivity}
		<div class="task-action">
			<MaterialIconButton
				size="toolbar"
				variant="ghost"
				label="打开任务"
				title={toolRunTitle}
				icon="listTodo"
				onclick={() => onOpenTasks?.()}
			/>
			{#if taskCount > 0}
				<span class="status-badge" aria-hidden="true">{taskCount}</span>
			{/if}
		</div>
	{/if}
</div>

<style>
	.status-switch {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		-webkit-app-region: no-drag;
	}
	.status-chip {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		height: var(--md-comp-status-height);
		min-width: var(--md-comp-button-touch-height);
		padding: 0 var(--md-comp-status-padding-inline);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-comp-status-radius);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		font-family: inherit;
	}
	.task-action {
		position: relative;
		display: inline-flex;
		align-items: center;
	}
	.status-badge {
		position: absolute;
		top: -3px;
		right: -3px;
		min-width: 16px;
		height: 16px;
		padding: 0 var(--md-sys-space-xs);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-tertiary);
		color: var(--md-sys-color-on-tertiary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: 16px;
		text-align: center;
		font-variant-numeric: tabular-nums;
		pointer-events: none;
	}
	.recording-text {
		color: var(--md-sys-color-error);
	}
</style>
