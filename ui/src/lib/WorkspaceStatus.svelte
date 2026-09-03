<script>
	import StatusDot from './StatusDot.svelte';
	import { isPausedStatus } from './sessionStatus.ts';

	let {
		overlay = {},
		modelState = 'ready',
		busySessions = new Set(),
		bootstrapReady = true,
		llmConnected = null,
		awaitingBackgroundActive = false,
		actionMenuOpen = false,
		runningActionCount = 0,
		pendingScheduledActions = [],
		runningSessions = [],
		runningBackgroundActions = [],
		completedActions = [],
		activeSessionId = null,
		sessions = [],
		onToggle = () => {},
		onCancel = () => {},
		onDeleteHistory = () => {},
		actionStatusLabel = /** @type {(status: string) => string} */ ((status) => status || ''),
		actionStatusColor = () => 'var(--md-sys-color-outline)',
		sessionTitleFor = () => '',
		actionDuration = () => '',
		scheduledActionCountdown = () => '',
		formatHistoryTime = () => '',
		historyTitleTone = () => 'muted',
	} = $props();

	const statusLabel = $derived.by(() => {
		if (overlay.isRecording) return '录音中';
		if (overlay.processing) return '转写中';
		if (modelState === 'streaming') return '生成中';
		if (modelState === 'tool') return '工具调用';
		if (modelState === 'balanced_model') return '备用模型';
		if (modelState === 'stalled' || modelState === 'waiting') return '等待响应';
		if (busySessions.size > 0) return `${busySessions.size} 个会话运行中`;
		if (awaitingBackgroundActive) return '等待后台';
		if (runningActionCount > 0) return '后台任务';
		if (!bootstrapReady) return '加载中';
		if (llmConnected === 'unconfigured') return '未配置';
		if (llmConnected === 'disconnected') return '已断开';
		if (llmConnected === 'ready') return '就绪';
		return '检测中';
	});

	const statusColor = $derived.by(() => {
		if (overlay.isRecording) return 'error';
		if (overlay.processing || modelState === 'stalled' || modelState === 'waiting' || busySessions.size > 0) return 'warning';
		if (modelState === 'streaming') return 'primary';
		if (modelState === 'tool' || awaitingBackgroundActive) return 'tertiary';
		if (runningActionCount > 0 || llmConnected === 'ready') return 'success';
		return 'outline';
	});

	const statusTitle = $derived(
		runningActionCount > 0 || pendingScheduledActions.length > 0
			? `任务${runningActionCount > 0 ? `（${runningActionCount} 个后台任务运行中）` : ''}${pendingScheduledActions.length > 0 ? `· 定时任务（${pendingScheduledActions.length} 条）` : ''}`
			: '任务：后台任务与定时任务',
	);

	/** @param {any} session */
	function sessionStatusLabel(session) {
		if (session.status === 'running') return '运行中';
		if (
			session.status === 'paused' &&
			runningBackgroundActions.some((action) => action.sessionId === session.id)
		)
			return '等待后台';
		return isPausedStatus(session.status) ? '已暂停' : '等待中';
	}
</script>

<div class="status-switch">
	<button
		class="status-chip status-chip-btn"
		onclick={() => onToggle?.()}
		title={statusTitle}
		aria-label="任务：后台任务与定时任务"
		type="button"
	>
		<StatusDot color={statusColor} animate={statusLabel !== '就绪' && statusLabel !== '未配置' && statusLabel !== '已断开'} />
		<span class:recording-text={overlay.isRecording} class="status-text">{statusLabel}</span>
		{#if runningActionCount > 0 || pendingScheduledActions.length > 0}
			<span class="status-badge" class:status-badge-running={runningActionCount > 0}>{runningActionCount + pendingScheduledActions.length}</span>
		{/if}
	</button>
	{#if actionMenuOpen}
		<div class="status-action-menu action-menu" role="dialog" aria-label="任务摘要">
			<div class="action-menu-title">正在运行</div>
			{#if runningSessions.length === 0 && runningBackgroundActions.length === 0}
				<div class="action-menu-empty">暂无运行中的任务</div>
			{:else}
				{#if runningSessions.length > 0}
					<div class="action-menu-subtitle">前台（会话）</div>
					{#each runningSessions as session (session.id)}
						<div class="action-item" class:action-item-running={session.status === 'running'}>
							<span class="action-dot" style="color: {session.status === 'running' ? 'var(--md-sys-color-success)' : 'var(--md-sys-color-warning)'}">&#9679;</span>
							<div class="action-item-main">
								<div class="action-item-top">
									<span class="action-id">{session.title || session.input}</span>
									<span class="action-item-status" class:running={session.status === 'running'}>{sessionStatusLabel(session)}</span>
								</div>
								<div class="action-item-sub"><span class="action-session">{session.id}</span></div>
							</div>
						</div>
					{/each}
				{/if}
				{#if runningBackgroundActions.length > 0}
					<div class="action-menu-subtitle">后台（任务）</div>
					{#each runningBackgroundActions as action (action.id)}
						<div class="action-item action-item-running">
							<span class="action-dot" style="color: {actionStatusColor(action.status)}">&#9679;</span>
							<div class="action-item-main">
								<div class="action-item-top"><span class="action-id">{action.id}</span><span class="action-item-status running">{actionStatusLabel(action.status)}</span></div>
								<div class="action-item-sub"><span class="action-session">{sessionTitleFor(action)}</span><span class="action-duration">{actionDuration(action)}</span></div>
								{#if action.output}<div class="action-output">{action.output}</div>{/if}
							</div>
							<button class="action-cancel" onclick={() => onCancel(action.id, 'background')} title="停止后台任务" aria-label="停止后台任务" type="button">&#x2715;</button>
						</div>
					{/each}
				{/if}
			{/if}
			<div class="action-menu-title scheduled-menu-title">定时任务</div>
			{#if pendingScheduledActions.length === 0}
				<div class="action-menu-empty">暂无定时任务</div>
			{:else}
				{#each pendingScheduledActions as action (action.id)}
					<div class="action-item scheduled-item">
						<span class="scheduled-dot">&#9200;</span>
						<div class="action-item-main">
							<div class="action-item-top"><span class="scheduled-title-text">{action.title || action.body}</span><span class="action-item-status">{action.mode === 'continue' ? '续接会话' : '执行工具'}</span></div>
							<div class="action-item-sub"><span class="scheduled-body">{action.body}</span><span class="action-duration">{scheduledActionCountdown(action.dueAt)}</span></div>
						</div>
						<button class="action-cancel" onclick={() => onCancel(action.id, 'scheduled')} title="取消定时任务" aria-label="取消定时任务" type="button">&#x2715;</button>
					</div>
				{/each}
			{/if}
			<div class="action-menu-title scheduled-menu-title">本会话已完成</div>
			{#if completedActions.length === 0}
				<div class="action-menu-empty">{activeSessionId ? '本会话暂无已完成的任务' : '请先打开一个会话'}</div>
			{:else}
				{#each completedActions as action (action.id)}
					{@const tone = historyTitleTone(action)}
					<div class="action-item scheduled-item history-item history-item--{tone}">
						<div class="action-item-main">
							<div class="action-item-top"><span class="scheduled-title-text history-title history-title--{tone}">{action.kind === 'scheduled' ? action.title || action.body || '定时任务' : action.command || action.id}</span><span class="action-item-status history-status history-status--{tone}">{action.kind === 'scheduled' ? action.mode === 'continue' ? '已续接会话' : '已执行' : actionStatusLabel(action.status)}</span></div>
							<div class="action-item-sub"><span class="scheduled-body">{action.kind === 'scheduled' ? action.body : action.output || action.errorReason || action.id}</span><span class="action-duration">{formatHistoryTime(action)}</span></div>
						</div>
						<button class="action-cancel" onclick={() => onDeleteHistory(action.id)} title="删除记录" aria-label="删除记录" type="button">&#x2715;</button>
					</div>
				{/each}
			{/if}
		</div>
	{/if}
</div>

<style>
	.status-switch { position: relative; -webkit-app-region: no-drag; }
	.status-chip { display: inline-flex; align-items: center; gap: var(--md-sys-space-sm); height: 34px; padding: 0 var(--md-sys-space-md); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font-size: 12px; font-weight: 600; cursor: pointer; font-family: inherit; transition: background var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard); }
	.status-chip-btn:hover { background: var(--md-sys-color-surface-container-highest); }
	.status-badge { min-width: 16px; height: 16px; padding: 0 4px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-tertiary); color: var(--md-sys-color-on-tertiary); font-size: 10px; font-weight: 700; line-height: 16px; text-align: center; font-variant-numeric: tabular-nums; }
	.status-badge-running { background: var(--md-sys-color-success-container); color: var(--md-sys-color-on-success-container); }
	.recording-text { color: var(--md-sys-color-error); }
	.action-menu { position: absolute; right: 0; top: calc(100% + 8px); z-index: 1000; min-width: 280px; max-width: 360px; max-height: 360px; overflow-y: auto; background: var(--md-sys-color-surface-container-high); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); padding: var(--md-sys-space-xs); box-shadow: var(--md-sys-elevation-2); }
	.action-menu-subtitle, .action-menu-title { color: var(--md-sys-color-on-surface-variant); padding: var(--md-sys-space-xs) var(--md-sys-space-md); }
	.action-menu-subtitle { font-size: 11px; font-weight: 600; }
	.action-menu-title { font-size: 11px; font-weight: 600; letter-spacing: 0.4px; text-transform: uppercase; padding-block: var(--md-sys-space-sm); }
	.action-menu-empty { padding: var(--md-sys-space-lg) var(--md-sys-space-md); font-size: 12px; color: var(--md-sys-color-on-surface-variant); text-align: center; }
	.action-item { display: flex; align-items: flex-start; gap: var(--md-sys-space-sm); padding: var(--md-sys-space-sm) var(--md-sys-space-md); border-radius: var(--md-sys-shape-small); opacity: 0.85; }
	.action-item-running { opacity: 1; background: var(--md-sys-color-surface-container); }
	.action-dot { font-size: 10px; margin-top: 3px; flex-shrink: 0; }
	.action-item-main { flex: 1; min-width: 0; }
	.action-item-top, .action-item-sub { display: flex; align-items: center; gap: var(--md-sys-space-sm); min-width: 0; }
	.action-item-sub { margin-top: 2px; }
	.action-id { font-size: 11px; font-family: var(--md-sys-typescale-mono); color: var(--md-sys-color-on-surface); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.action-item-status { flex-shrink: 0; font-size: 10px; color: var(--md-sys-color-on-surface-variant); margin-left: auto; }
	.action-item-status.running { color: var(--md-sys-color-success); }
	.action-session, .scheduled-body { font-size: 11px; color: var(--md-sys-color-on-surface-variant); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.action-duration { font-size: 10px; color: var(--md-sys-color-on-surface-variant); margin-left: auto; flex-shrink: 0; font-family: var(--md-sys-typescale-mono); }
	.action-output { margin-top: 4px; max-height: 160px; overflow: auto; font-size: 10px; font-family: var(--md-sys-typescale-mono); line-height: 1.45; color: var(--md-sys-color-on-surface-variant); white-space: pre-wrap; word-break: break-all; opacity: 0.9; padding: var(--md-sys-space-xs) var(--md-sys-space-sm); border-radius: var(--md-sys-shape-extra-small); background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent); }
	.scheduled-menu-title { margin-top: var(--md-sys-space-xs); border-top: 1px solid var(--md-sys-color-outline-variant); }
	.scheduled-dot { font-size: 12px; line-height: 1; margin-top: 2px; flex-shrink: 0; }
	.scheduled-title-text { font-size: 12px; font-weight: 600; color: var(--md-sys-color-on-surface); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.history-title--ok, .history-status--ok { color: var(--md-sys-color-success); }
	.history-title--fail, .history-status--fail { color: var(--md-sys-color-error); }
	.history-title--muted, .history-status--muted { color: var(--md-sys-color-on-surface-variant); }
	.history-status--ok, .history-status--fail { font-weight: 600; }
	.history-item--fail { background: color-mix(in srgb, var(--md-sys-color-error) 8%, transparent); }
	.history-item--ok { background: color-mix(in srgb, var(--md-sys-color-success) 6%, transparent); }
	.action-cancel { flex-shrink: 0; width: var(--md-sys-icon-button-size); height: var(--md-sys-icon-button-size); border: none; border-radius: var(--md-sys-shape-small); background: transparent; color: var(--md-sys-color-error); font-size: 12px; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: background var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard); }
	.action-cancel:hover { background: var(--md-sys-color-error-container); }
</style>
