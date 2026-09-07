<script>
	import MaterialButton from './MaterialButton.svelte';
	import StatusDot from './StatusDot.svelte';
	import { taskKindLabel } from '$lib/taskTerminology.ts';

	let {
		overlay = {},
		modelState = 'ready',
		busySessions = new Set(),
		runtime = 'tauri',
		bootstrapReady = true,
		llmConnected = null,
		awaitingBackgroundActive = false,
		runningActionCount = 0,
		pendingScheduledActions = [],
		onOpenTasks = () => {},
	} = $props();

	const statusLabel = $derived.by(() => {
		if (runtime === 'browser') return '浏览器预览';
		if (overlay.isRecording) return '录音中';
		if (overlay.processing) return '转写中';
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

	const statusColor = $derived.by(() => {
		if (runtime === 'browser') return 'outline';
		if (overlay.isRecording) return 'error';
		if (
			overlay.processing ||
			modelState === 'stalled' ||
			modelState === 'waiting' ||
			busySessions.size > 0
		)
			return 'warning';
		if (modelState === 'streaming') return 'primary';
		if (modelState === 'tool' || awaitingBackgroundActive) return 'tertiary';
		if (runningActionCount > 0 || llmConnected === 'ready') return 'success';
		return 'outline';
	});

	const statusTitle = $derived.by(() => {
		if (runtime === 'browser') {
			return '当前是浏览器预览，Rust/Tauri 后端未启动；运行 cargo tauri dev 启动桌面应用';
		}
		const parts = [];
		if (runningActionCount > 0) {
			parts.push(`${runningActionCount} 个${taskKindLabel('background')}运行中`);
		}
		if (pendingScheduledActions.length > 0) {
			parts.push(`${pendingScheduledActions.length} 条${taskKindLabel('scheduled')}`);
		}
		return parts.length > 0 ? `任务：${parts.join('，')}` : '打开任务查看任务状态';
	});
</script>

<div class="status-switch">
	<MaterialButton
		variant="outlined"
		className="status-chip"
		onclick={() => onOpenTasks?.()}
		title={statusTitle}
		ariaLabel={`应用状态：${statusLabel}，打开任务`}
	>
		<StatusDot
			color={statusColor}
			animate={
				statusLabel !== '就绪' &&
				statusLabel !== '未配置' &&
				statusLabel !== '已断开' &&
				statusLabel !== '浏览器预览'
			}
		/>
		<span class:recording-text={overlay.isRecording} class="status-text">{statusLabel}</span>
		{#if runningActionCount > 0 || pendingScheduledActions.length > 0}
			<span class="status-badge">{runningActionCount + pendingScheduledActions.length}</span>
		{/if}
	</MaterialButton>
</div>

<style>
	.status-switch {
		position: relative;
		-webkit-app-region: no-drag;
	}
	:global(.status-chip) {
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
		cursor: pointer;
		font-family: inherit;
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	:global(.status-chip:hover) {
		background: var(--md-sys-color-surface-container-highest);
	}
	.status-badge {
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
	}
	.recording-text {
		color: var(--md-sys-color-error);
	}
</style>
