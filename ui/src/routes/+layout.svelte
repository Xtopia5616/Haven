<script>
	import '../app.css';
	import { addNotification, recordingOverlay, newMessage, taskMessagesStore, addTaskMessage, activeTaskIdStore, modelStateStore, updateModelState, clearModelStateTimer } from '$lib/stores.js';
	import { themeStore } from '$lib/themeStore.js';
	import { listen, invoke } from '$lib/tauri.js';
	import logger from '$lib/logger.js';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { fade } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { page } from '$app/stores';

	import RecordingIndicator from '$lib/RecordingIndicator.svelte';
	import Logo from '$lib/Logo.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import NotificationToast from '$lib/NotificationToast.svelte';

	let { children } = $props();
	let activeTab = $state('chat');
	let theme = $state(themeStore.currentTheme);
	themeStore.subscribe((v) => theme = v.theme);

	let overlay = $state({
		visible: false,
		isRecording: false,
		processing: false,
		sessionId: null,
		startedAt: null,
		reason: null,
		vadState: 'silent',
	});
	let duration = $state(0);
	let durationTimer;
	let processingTimer;
	let modelState = $state('ready'); // synced from modelStateStore on mount
	// Probe state is declared BEFORE the subscribe below: the store's
	// `subscribe` fires synchronously (SSR/mount) with the current value, and
	// `probeLlmConnection` reads these bindings without awaiting first, so
	// they must be initialized already.
	let llmConnected = $state(null);
	let llmProbeTimer;
	let llmProbeInFlight = false;
	let llmProbeFailureStreak = 0;
	const LLM_PROBE_INTERVAL_MS = 15000;
	const LLM_PROBE_MAX_INTERVAL_MS = 120000;
	modelStateStore.subscribe((v) => {
		modelState = v;
		if (v === 'ready') probeLlmConnection();
	});

	async function probeLlmConnection() {
		if (modelState !== 'ready' || llmProbeInFlight) return;
		llmProbeInFlight = true;
		try {
			llmConnected = await invoke('check_llm_connection');
			llmProbeFailureStreak = 0;
		} catch (e) {
			logger.warn('+layout', 'check_llm_connection error', e);
			llmConnected = false;
			llmProbeFailureStreak = Math.min(llmProbeFailureStreak + 1, 4);
		} finally {
			llmProbeInFlight = false;
		}
	}

	// Adaptive schedule: back off on consecutive failures (15s → 30s → 60s →
	// 120s cap), reset to 15s after a successful probe. A dead endpoint no
	// longer causes an unconditional multi-second network request every 15s.
	function scheduleLlmProbe() {
		const interval =
			llmProbeFailureStreak === 0
				? LLM_PROBE_INTERVAL_MS
				: Math.min(
						LLM_PROBE_INTERVAL_MS * 2 ** llmProbeFailureStreak,
						LLM_PROBE_MAX_INTERVAL_MS,
					);
		llmProbeTimer = setTimeout(() => {
			probeLlmConnection();
			scheduleLlmProbe();
		}, interval);
	}

	let notifyCfg = $state({
		task_created: { in_app: true },
		task_completed: { in_app: true },
		task_paused: { in_app: true },
		task_error: { in_app: true },
	});

	// The configured recording hotkey binding (e.g. "Ctrl+Shift+Space"),
	// loaded from settings so the statusbar hint reflects the real value
	// instead of a hardcoded string. Updated live on `hotkey:rebind`.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	recordingOverlay.subscribe((v) => (overlay = v));

	$effect(() => {
		if (typeof window !== 'undefined') {
			const path = $page.url.pathname;
			if (path === '/tools') activeTab = 'tools';
			else if (path === '/history') activeTab = 'history';
			else if (path === '/settings') activeTab = 'settings';
			else activeTab = 'chat';
		}
	});

	function setOverlay(patch) {
		recordingOverlay.update((v) => ({ ...v, ...patch }));
	}

	function startTimer() {
		if (durationTimer) clearInterval(durationTimer);
		duration = 0;
		durationTimer = setInterval(() => {
			duration += 1;
		}, 1000);
	}

	function stopTimer() {
		if (durationTimer) clearInterval(durationTimer);
		durationTimer = null;
	}

	function closeOverlaySoon(ms = 1500) {
		if (processingTimer) clearTimeout(processingTimer);
		processingTimer = setTimeout(() => {
			setOverlay({
				visible: false,
				isRecording: false,
				processing: false,
				reason: null,
			});
			stopTimer();
		}, ms);
	}

	async function cancelRecording() {
		try {
			await invoke('cancel_recording');
		} catch (e) {
			addNotification(`停止录音失败: ${e}`, 'error', 3000);
		}
		setOverlay({
			visible: false,
			isRecording: false,
			processing: false,
			reason: null,
		});
		stopTimer();
	}

	function toggleTheme() {
		themeStore.toggle();
		theme = themeStore.currentTheme;
	}

	let unlisteners = [];

	async function safeListen(event, handler) {
		try {
			const unsub = await listen(event, handler);
			unlisteners.push(unsub);
		} catch (e) {
			logger.error('+layout', `Failed to register listener for '${event}'`, e);
		}
	}

	onMount(async () => {
		// Load notify config + hotkey binding in background — don't block
		// listener registration.
		invoke('get_settings').then((settings) => {
			if (settings?.notification) {
				notifyCfg = { ...notifyCfg, ...settings.notification };
			}
			if (settings?.hotkey?.key_binding) {
				hotkeyBinding = settings.hotkey.key_binding;
			}
		}).catch((e) => {
			logger.warn('+layout', 'get_settings error', e);
		});

		await safeListen('recording:started', (event) => {
			const data = event.payload || {};
			setOverlay({
				visible: true,
				isRecording: true,
				processing: false,
				sessionId: data.session_id || null,
				startedAt: Date.now(),
				reason: null,
				vadState: 'silent',
			});
			startTimer();
		});
		await safeListen('recording:stopped', (event) => {
			const data = event.payload || {};
			if (processingTimer) clearTimeout(processingTimer);
			const reason = data.reason || null;
			const isAuto = reason === 'silence' || reason === 'max_duration';
			setOverlay({
				isRecording: false,
				processing: isAuto,
				reason,
				vadState: 'silent',
			});
			stopTimer();
			if (reason === 'cancel') {
				setOverlay({ visible: false, processing: false });
			}
		});
		await safeListen('recording:vad_status', (event) => {
			const data = event.payload || {};
			if (get(recordingOverlay).isRecording) {
				setOverlay({ vadState: data.state || 'silent' });
			}
		});
		await safeListen('recording:error', (event) => {
			const data = event.payload || {};
			addNotification(data.error || '录音错误，请检查麦克风/STT 配置', 'error', 5000);
			setOverlay({
				visible: false,
				isRecording: false,
				processing: false,
				reason: null,
			});
			stopTimer();
		});
		await safeListen('transcription:result', (event) => {
			const data = event.payload || {};
			const text = (data.text || '').trim();
			if (text) {
				const activeId = get(activeTaskIdStore);
				const taskId = activeId || '_draft';
				addTaskMessage(taskId, newMessage({ role: 'user', content: text, voice: true, time: new Date().toLocaleTimeString() }));
			} else {
				// 转写为空：静音或过短的录音没有产出任何内容，必须给用户
				// 明确反馈，否则看起来像"点了没反应"。
				const durationMs = data.duration_ms || 0;
				if (durationMs > 0 && durationMs < 1000) {
					addNotification('录音时间太短，请再试一次', 'warning', 3000);
				} else {
					addNotification('未检测到语音，请再试一次', 'error', 4000);
				}
			}
			setOverlay({
				visible: false,
				isRecording: false,
				processing: false,
				reason: null,
			});
			stopTimer();
		});
		await safeListen('transcription:error', (event) => {
			const data = event.payload || {};
			addNotification(data.error || '转写失败，请检查 STT 服务配置', 'error', 5000);
			setOverlay({
				visible: false,
				isRecording: false,
				processing: false,
				reason: null,
			});
			stopTimer();
		});
		await safeListen('mute:changed', (event) => {
			const data = event.payload || {};
			if (data.muted) {
				addNotification('麦克风已静音', 'info');
				if (get(recordingOverlay).isRecording) {
					addNotification('录音被静音强制停止', 'warning', 4000);
					setOverlay({
						visible: false,
						isRecording: false,
						processing: false,
						reason: 'muted',
					});
					stopTimer();
				}
			} else {
				addNotification('麦克风已取消静音', 'info');
			}
		});
		await safeListen('tray:status_changed', (event) => {
			const data = event.payload || {};
			if (data.status === 'muted' && get(recordingOverlay).isRecording) {
				setOverlay({
					visible: false,
					isRecording: false,
					processing: false,
					reason: 'muted',
				});
				stopTimer();
			}
		});
		await safeListen('hotkey:conflict', (event) => {
			const data = event.payload || {};
			addNotification(
				`Hotkey conflict: ${data.binding} - ${data.error}`,
				'error',
				5000,
			);
		});
		await safeListen('hotkey:rebind', (event) => {
			const data = event.payload || {};
			if (data.new_binding) {
				hotkeyBinding = data.new_binding;
			}
		});
		await safeListen('task:created', (event) => {
			const data = event.payload;
			const title = data.title || data.task_id;
			if (notifyCfg?.task_created?.in_app !== false) {
				addNotification(`新任务: ${title}`, 'info', 4000);
			}
			updateModelState('waiting', { idleTimeoutMs: 5000 });
		});
		await safeListen('task:completed', (event) => {
			const data = event.payload;
			const title = data.title || data.task_id;
			if (notifyCfg?.task_completed?.in_app !== false) {
				addNotification(`任务已完成: ${title}`, 'success');
			}
			updateModelState('ready');
		});
		await safeListen('task:error', (event) => {
			const data = event.payload;
			const errMsg = data.error || data.task_id;
			if (notifyCfg?.task_error?.in_app !== false) {
				addNotification(`任务出错: ${errMsg}`, 'error', 5000);
			}
			clearModelStateTimer();
			updateModelState('ready');
		});
		await safeListen('task:updated', (event) => {
			const data = event.payload;
			const title = data.title || data.task_id;
			if (data.status === 'paused') {
				if (notifyCfg?.task_paused?.in_app !== false) {
					addNotification(`任务已暂停: ${title || '未知'}`, 'warning', 3000);
				}
				clearModelStateTimer();
				updateModelState('ready');
			}
			if (data.status === 'pending') {
				if (notifyCfg?.task_paused?.in_app !== false) {
					addNotification(`任务已恢复: ${title || '未知'}`, 'info', 3000);
				}
				updateModelState('waiting', { idleTimeoutMs: 5000 });
			}
			if (data.status === 'completed') {
				clearModelStateTimer();
				updateModelState('ready');
			}
			if (data.status === 'error') {
				clearModelStateTimer();
				updateModelState('ready');
			}
		});
		await safeListen('mcp:status_change', (event) => {
			const data = event.payload;
			const name = data.name || '';
			const status = data.status;
			if (status === 'Connected') {
				addNotification(`MCP 已连接: ${name}`, 'success', 3000);
			} else if (status === 'Disconnected') {
				addNotification(`MCP 已断开: ${name}`, 'warning', 4000);
			} else if (status && status.Offline) {
				const err = status.Offline.error || '';
				addNotification(`MCP 离线: ${name}${err ? ` - ${err}` : ''}`, 'error', 5000);
			} else if (status === 'Connecting') {
				addNotification(`MCP 连接中: ${name}`, 'info', 2000);
			}
		});
		await safeListen('skills:status_change', () => {
			// Skill list refresh is notified by the tools page refresh button.
		});
		await safeListen('agent:balanced_model', (event) => {
			const data = event.payload;
			const activeId = get(activeTaskIdStore);
			if (data.task_id && activeId && data.task_id !== activeId) return;
			updateModelState('balanced_model');
			addNotification(`Balanced Model: ${data.reason}`, 'warning');
		});
		await safeListen('notification:show', (event) => {
			const data = event.payload || {};
			const title = data.title || 'Haven';
			const body = data.body || '新通知';
			// When the title is the default "Haven", showing "Haven: msg" is
			// redundant — the toast itself already lives in the app.
			addNotification(title === 'Haven' ? body : `${title}: ${body}`, 'info', 5000);
		});

		probeLlmConnection();
		scheduleLlmProbe();
	});

	onDestroy(() => {
		stopTimer();
		if (processingTimer) clearTimeout(processingTimer);
		if (llmProbeTimer) clearTimeout(llmProbeTimer);
		clearModelStateTimer();
		unlisteners.forEach((u) => u && u());
	});

	const tabs = [
		{ id: 'chat', label: '对话', href: '/' },
		{ id: 'tools', label: '工具', href: '/tools' },
		{ id: 'history', label: '历史', href: '/history' },
		{ id: 'settings', label: '设置', href: '/settings' },
	];
</script>

<div class="app-layout">
	<header class="titlebar">
		<div class="titlebar-left">
			<Logo size={22} withText={true} />
		</div>
		<div class="titlebar-right">
			<span class="status-chip" aria-live="polite">
				{#if overlay.isRecording}
					<StatusDot color="error" animate={true} />
					<span class="status-text recording-text">Recording</span>
				{:else if overlay.processing}
					<StatusDot color="warning" animate={true} />
					<span class="status-text">Transcribing</span>
				{:else if modelState === 'waiting'}
					<StatusDot color="warning" animate={true} />
					<span class="status-text">Waiting for model...</span>
				{:else if modelState === 'streaming'}
					<StatusDot color="success" animate={true} />
					<span class="status-text">Generating</span>
				{:else if modelState === 'tool'}
					<StatusDot color="warning" animate={true} />
					<span class="status-text">Tool</span>
				{:else if modelState === 'balanced_model'}
					<StatusDot color="error" animate={true} />
					<span class="status-text">Balanced Model</span>
				{:else if llmConnected === false}
					<StatusDot color="outline" />
					<span class="status-text">Disconnected</span>
				{:else}
					<StatusDot color="success" />
					<span class="status-text">Ready</span>
				{/if}
			</span>
			<button
				class="md-icon-button theme-toggle"
				onclick={toggleTheme}
				aria-label="Toggle theme"
				title={theme === 'dark' ? '切换到亮色模式' : '切换到暗色模式'}
			>
				{#if theme === 'dark'}
					<svg viewBox="0 0 24 24" fill="currentColor">
						<path
							d="M12 7a5 5 0 100 10 5 5 0 000-10zm0-5a1 1 0 011 1v2a1 1 0 11-2 0V3a1 1 0 011-1zm0 17a1 1 0 011 1v2a1 1 0 11-2 0v-2a1 1 0 011-1zM4.2 4.2a1 1 0 011.4 0l1.5 1.5A1 1 0 015.7 7.1L4.2 5.6a1 1 0 010-1.4zm12.7 12.7a1 1 0 011.4 0l1.5 1.5a1 1 0 11-1.4 1.4l-1.5-1.5a1 1 0 010-1.4zM2 12a1 1 0 011-1h2a1 1 0 110 2H3a1 1 0 01-1-1zm17 0a1 1 0 011-1h2a1 1 0 110 2h-2a1 1 0 01-1-1zM4.2 19.8a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0zm12.7-12.7a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0z"
						/>
					</svg>
				{:else}
					<svg viewBox="0 0 24 24" fill="currentColor">
						<path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z" />
					</svg>
				{/if}
			</button>
		</div>
	</header>

	<nav class="md-tabs tabbar" aria-label="Primary">
		{#each tabs as tab}
			<a
				href={tab.href}
				class="md-tab"
				class:active={activeTab === tab.id}
				aria-current={activeTab === tab.id ? 'page' : undefined}
				onclick={() => {
					activeTab = tab.id;
				}}
			>
				<span class="tab-label">{tab.label}</span>
			</a>
		{/each}
	</nav>

	<main class="content" class:content--chat={$page.url.pathname === '/'}>
		{#key $page.url.pathname}
			<div class="page-shell" in:fade={{ duration: 280, easing: cubicOut }}>
				{@render children()}
			</div>
		{/key}
	</main>

	<RecordingIndicator
		isRecording={overlay.isRecording}
		processing={overlay.processing}
		{duration}
		vadState={overlay.vadState}
		reason={overlay.reason}
		onCancel={cancelRecording}
	/>

	<NotificationToast />

	<footer class="statusbar">
		<span class="hotkey-hint">{hotkeyBinding} 开始录音</span>
		{#if overlay.isRecording}
			<span class="recording-label">Recording…</span>
		{/if}
	</footer>
</div>

<style>
	.app-layout {
		display: flex;
		flex-direction: column;
		height: 100vh;
		background: var(--md-sys-color-background);
		color: var(--md-sys-color-on-surface);
	}

	.titlebar {
		height: 52px;
		background: var(--md-sys-color-surface-container);
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0 var(--md-sys-space-lg);
		flex-shrink: 0;
		-webkit-app-region: drag;
	}
	.titlebar-right {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		-webkit-app-region: no-drag;
	}
	.status-chip {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		height: 32px;
		padding: 0 var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-weight: 600;
	}
	.recording-text {
		color: var(--md-sys-color-error);
	}
	.theme-toggle {
		color: var(--md-sys-color-on-surface-variant);
	}

	.tabbar {
		flex-shrink: 0;
	}
	.md-tabs {
		/* ensure tab row spans full width; .md-tabs from app.css sets fixed 48px height */
		width: 100%;
	}

	.content {
		flex: 1;
		overflow-y: auto;
		padding: var(--md-sys-space-2xl);
		background: var(--md-sys-color-surface);
	}
	.content--chat {
		overflow: hidden;
		padding: 0;
		display: flex;
		flex-direction: column;
	}
	.page-shell {
		width: 100%;
		margin: 0 auto;
	}
	/* Non-chat routes: cap content at the shared content max-width and
	 * let it grow with viewport (clamp ensures a sensible minimum on
	 * narrow windows and a consistent cap on wide ones). Chat opts out
	 * via .content--chat and uses its own internal constraints. */
	.content:not(.content--chat) .page-shell {
		max-width: clamp(640px, 92vw, var(--md-sys-content-max-width));
	}

	.content--chat .page-shell {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}

	.statusbar {
		height: 32px;
		background: var(--md-sys-color-surface-container);
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0 var(--md-sys-space-lg);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		flex-shrink: 0;
	}
	.hotkey-hint {
		font-weight: 600;
		letter-spacing: 0.2px;
	}
	.recording-label {
		color: var(--md-sys-color-error);
		font-weight: 700;
	}
</style>