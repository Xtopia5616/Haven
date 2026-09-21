<script>
	import MaterialIconButton from './MaterialIconButton.svelte';
	import StatusDot from './StatusDot.svelte';
	import { taskKindLabel } from '$lib/taskTerminology.ts';

	let {
		overlay = {},
		modelState = 'ready',
		busySessions = new Set(),
		conversationStatus = '就绪',
		runtime = 'tauri',
		bootstrapReady = true,
		llmConnected = null,
		llmConnectionDetail = null,
		awaitingBackgroundActive = false,
		runningActionCount = 0,
		pendingScheduledActions = [],
		onOpenTasks = () => {},
	} = $props();

	const statusLabel = $derived.by(() => {
		if (runtime === 'browser') return '浏览器预览';
		if (overlay.isRecording) return '录音中';
		if (overlay.processing) return '转写中';
		// Mirror the selected conversation in the shell. With parallel sessions,
		// keep the global count so the titlebar does not hide other work.
		if (conversationStatus !== '就绪' && busySessions.size <= 1) return conversationStatus;
		if (modelState === 'streaming') return '生成中';
		if (modelState === 'tool') return '工具调用';
		if (modelState === 'stalled' || modelState === 'waiting') return '等待响应';
		if (busySessions.size > 0) return `${busySessions.size} 个会话运行中`;
		if (awaitingBackgroundActive) return `等待${taskKindLabel('background')}`;
		if (runningActionCount > 0) return taskKindLabel('background');
		if (!bootstrapReady) return '加载中';
		if (llmConnected === 'unconfigured') return '未配置';
		if (llmConnected === 'disconnected') return '已断开';
		if (llmConnected === 'ready') return '就绪';
		return '检测中';
	});

	const taskCount = $derived(runningActionCount + pendingScheduledActions.length);
	const hasTaskActivity = $derived(taskCount > 0 || awaitingBackgroundActive);

	const statusColor = $derived.by(() => {
		if (runtime === 'browser') return 'neutral';
		if (overlay.isRecording) return 'error';
		if (conversationStatus === '已暂停') return 'warning';
		if (conversationStatus === '等待后台任务') return 'tool';
		if (conversationStatus === '运行中' || conversationStatus === '等待中') return 'warning';
		if (
			overlay.processing ||
			modelState === 'stalled' ||
			modelState === 'waiting' ||
			busySessions.size > 0
		)
			return 'warning';
		if (modelState === 'streaming') return 'info';
		if (modelState === 'tool' || awaitingBackgroundActive) return 'tool';
		if (runningActionCount > 0 || llmConnected === 'ready') return 'success';
		return 'neutral';
	});

	const statusAnimating = $derived(
		!['就绪', '未配置', '已断开', '浏览器预览', '已暂停'].includes(statusLabel),
	);

	const statusTitle = $derived.by(() => {
		if (runtime === 'browser') {
			return '当前是浏览器预览，Rust/Tauri 后端未启动；运行 cargo tauri dev 启动桌面应用';
		}
		if (llmConnected === 'disconnected') {
			return `模型连接失败：${llmConnectionDetail || '暂时无法确定具体原因'}。请到模型设置检查 API 地址、API Key 和代理`;
		}
		if (llmConnected === 'unconfigured') {
			return '默认模型未配置，请到模型设置填写 Provider、模型和 API Key';
		}
		const parts = [];
		if (runningActionCount > 0) {
			parts.push(`${runningActionCount} 个${taskKindLabel('background')}运行中`);
		}
		if (pendingScheduledActions.length > 0) {
			parts.push(`${pendingScheduledActions.length} 条${taskKindLabel('scheduled')}`);
		}
		return parts.length > 0 ? `任务：${parts.join('，')}` : `当前状态：${statusLabel}`;
	});

	const taskTitle = $derived.by(() => {
		const parts = [];
		if (runningActionCount > 0) parts.push(`${runningActionCount} 个后台任务运行中`);
		if (pendingScheduledActions.length > 0) {
			parts.push(`${pendingScheduledActions.length} 条${taskKindLabel('scheduled')}`);
		}
		return parts.length > 0 ? `打开任务：${parts.join('，')}` : '打开任务查看详情';
	});
</script>

<div class="status-switch">
	<div class="status-chip" role="status" aria-label={`应用状态：${statusLabel}`} title={statusTitle}>
		<StatusDot
			color={statusColor}
			animate={statusAnimating}
		/>
		<span class:recording-text={overlay.isRecording} class="status-text">{statusLabel}</span>
	</div>
	{#if hasTaskActivity}
		<div class="task-action">
			<MaterialIconButton
				size="toolbar"
				variant="ghost"
				label="打开任务"
				title={taskTitle}
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
