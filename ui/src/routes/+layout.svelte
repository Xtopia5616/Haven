<script>
	import '../app.css';
	import { addNotification, recordingOverlay, activeTaskIdStore, modelStateStore, updateModelState, clearModelStateTimer, upsertActivity, removeActivity, refreshActivities, activityStore, taskStore, cancelActivity, refreshActivityHistory, deleteActivity, formatMessageTime } from '$lib/stores.js';
	import { submitVoiceTranscript } from '$lib/voiceSubmit.js';
	import { themeStore } from '$lib/themeStore.js';
	import { invoke } from '$lib/tauri.js';
	import logger from '$lib/logger.js';
	import { registerListeners } from '$lib/events.js';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { fade } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { page } from '$app/stores';
	import { syncStore } from '$lib/syncStore.js';

	import RecordingIndicator from '$lib/RecordingIndicator.svelte';
	import Logo from '$lib/Logo.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import NotificationToast from '$lib/NotificationToast.svelte';

	let { children } = $props();
	let activeTab = $state('chat');
	let theme = $state(themeStore.currentTheme);
	$effect(() => syncStore(themeStore, (v) => theme = v.theme));

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
	// Whether ANY task is busy (pending/running). The model-state events only
	// fire while chunks flow; a task whose LLM call is stuck (idle timeout,
	// empty-response retries, provider hang) emits nothing, and the 5s idle
	// timer would flip the chip back to "就绪" mid-hang. taskBusy keeps the
	// chip truthful: driven by task:created / task:updated transitions, which
	// the backend emits on every status change (pending/running on submission,
	// paused/completed/error on termination). Tracked per task id so a
	// parallel task completing does not clear the busy state of another.
	let busyTasks = $state(new Set());
	const taskBusy = $derived(busyTasks.size > 0);
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
		task_resumed: { in_app: true },
		task_error: { in_app: true },
	});

	// The configured recording hotkey binding (e.g. "Ctrl+Shift+Space"),
	// loaded from settings so the statusbar hint reflects the real value
	// instead of a hardcoded string. Updated live on `hotkey:rebind`.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	$effect(() => syncStore(recordingOverlay, (v) => (overlay = v)));

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

	// Reset the recording overlay to its "hidden" state. Use after the user
	// finishes a session, errors out, or is force-stopped by mute/tray.
	function resetOverlay(reason = null) {
		setOverlay({ visible: false, isRecording: false, processing: false, reason });
		stopTimer();
	}

	function closeOverlaySoon(ms = 1500) {
		if (processingTimer) clearTimeout(processingTimer);
		processingTimer = setTimeout(() => {
			resetOverlay();
		}, ms);
	}

	async function cancelRecording() {
		try {
			await invoke('cancel_recording');
		} catch (e) {
			addNotification(`停止录音失败: ${e}`, 'error', 3000);
		}
		resetOverlay();
	}

	function toggleTheme() {
		themeStore.toggle();
		theme = themeStore.currentTheme;
	}

	// Activity registry (background jobs + reminders) mirrored from
	// activityStore (kept live by the `activity:*` listeners above). Jobs
	// sort newest-first; reminders sort soonest-first; both derive from one
	// store keyed by the normalized activity id. The status chip in the
	// titlebar opens a menu of these, replacing the old chat-toolbar button.
	let jobMenuOpen = $state(false);
	let activities = $state({});
	$effect(() => syncStore(activityStore, (v) => (activities = v)));
	const activityEntries = $derived(Object.values(activities));
	const jobEntries = $derived(
		activityEntries
			.filter((a) => a.kind === 'job')
			.sort((a, b) =>
				String(b.started_at || '').localeCompare(String(a.started_at || '')),
			),
	);
	const pendingReminders = $derived(
		activityEntries
			.filter((a) => a.kind === 'reminder')
			.sort((a, b) => String(a.due_at || '').localeCompare(String(b.due_at || ''))),
	);
	const runningJobCount = $derived(
		jobEntries.filter((j) => j.status === 'running').length,
	);

	// Panel tab filter: 'all' | 'job' | 'reminder' | 'history'.
	let activityTab = $state('all');

	// Fired-reminder history (and terminal job rows past the in-memory TTL),
	// fetched on demand when the history tab opens.
	let activityHistory = $state([]);
	let historyLoaded = $state(false);
	$effect(() => {
		if (!jobMenuOpen || activityTab !== 'history' || historyLoaded) return;
		refreshActivityHistory('reminder', 50).then((rows) => {
			activityHistory = rows;
			historyLoaded = true;
		});
	});

	// Task titles for job rows; mirrored from the chat page's loadTasks().
	let tasks = $state([]);
	$effect(() => syncStore(taskStore, (v) => (tasks = v)));

	// While the panel is open, re-render once a second so countdowns tick.
	let countdownTick = $state(0);
	$effect(() => {
		if (!jobMenuOpen) return;
		const t = setInterval(() => (countdownTick += 1), 1000);
		return () => clearInterval(t);
	});

	async function handleDeleteHistory(id) {
		try {
			await deleteActivity(id);
			activityHistory = activityHistory.filter((h) => h.id !== id);
			addNotification('已删除历史记录', 'success', 2000);
		} catch (e) {
			addNotification(`删除历史记录失败: ${e}`, 'error', 3000);
		}
	}

	function jobStatusLabel(status) {
		switch (status) {
			case 'running':
				return '运行中';
			case 'completed':
				return '已完成';
			case 'failed':
				return '失败';
			case 'cancelled':
				return '已取消';
			default:
				return status || '';
		}
	}

	function jobStatusColor(status) {
		switch (status) {
			case 'running':
				return '#44cc44';
			case 'completed':
				return '#4488ff';
			case 'failed':
				return '#ff4444';
			case 'cancelled':
				return '#888';
			default:
				return '#888';
		}
	}

	function taskTitleFor(job) {
		if (!job.task_id) return '';
		const t = tasks.find((x) => x.id === job.task_id);
		return t?.title || t?.input || job.task_id;
	}

	function jobDuration(job) {
		const start = new Date(job.started_at).getTime();
		if (isNaN(start)) return '';
		const end =
			job.status === 'running'
				? Date.now()
				: new Date(job.finished_at || job.started_at).getTime();
		if (isNaN(end)) return '';
		const secs = Math.floor((end - start) / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	async function handleCancelActivity(activityId, kind = 'job') {
		try {
			const ok = await cancelActivity(activityId, kind);
			if (!ok) {
				addNotification(
					kind === 'reminder' ? '提醒已触发或不存在' : '后台作业已结束，无需停止',
					'warning',
					2500,
				);
			}
		} catch (e) {
			addNotification(
				`${kind === 'reminder' ? '取消提醒' : '停止后台作业'}失败: ${e}`,
				'error',
				3000,
			);
		}
	}

	function reminderCountdown(dueAt) {
		const due = new Date(dueAt).getTime();
		if (isNaN(due)) return '';
		const diff = due - Date.now();
		if (diff <= 0) return '已到时间';
		const secs = Math.round(diff / 1000);
		if (secs < 60) return `${secs}s 后`;
		const mins = Math.floor(secs / 60);
		if (mins < 60) return `${mins}分后`;
		const hrs = Math.floor(mins / 60);
		if (hrs < 24) return `${hrs}小时${mins % 60}分后`;
		return `${Math.floor(hrs / 24)}天后`;
	}

	function formatHistoryTime(h) {
		const ts = h.finished_at || h.started_at || h.due_at;
		if (!ts) return '';
		const d = new Date(ts);
		if (isNaN(d.getTime())) return '';
		return formatMessageTime(d);
	}

	function handleWindowClick(e) {
		if (jobMenuOpen) {
			const menu = document.querySelector('.status-job-menu');
			const chip = document.querySelector('.status-chip-btn');
			if (menu && chip && !menu.contains(e.target) && !chip.contains(e.target)) {
				jobMenuOpen = false;
			}
		}
	}

	let eventRegistrations = null;

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

		const registrations = registerListeners({
			'recording:started': (event) => {
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
			},
			'recording:stopped': (event) => {
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
			},
			'recording:vad_status': (event) => {
				const data = event.payload || {};
				if (get(recordingOverlay).isRecording) {
					setOverlay({ vadState: data.state || 'silent' });
				}
			},
			'recording:error': (event) => {
				const data = event.payload || {};
				addNotification(data.error || '录音错误，请检查麦克风/STT 配置', 'error', 5000);
				resetOverlay();
			},
			'transcription:result': (event) => {
				const data = event.payload || {};
				const text = (data.text || '').trim();
				if (text) {
					// Same path as a typed message (see `submitVoiceTranscript`):
					// appends the voice message, submits with the current
					// `activeTaskId`, and migrates the message into the task if
					// the backend created a fresh one.
					submitVoiceTranscript(text).catch((e) =>
						addNotification(`语音提交失败: ${e}`, 'error', 5000)
					);
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
				resetOverlay();
			},
			'transcription:error': (event) => {
				const data = event.payload || {};
				addNotification(data.error || '转写失败，请检查 STT 服务配置', 'error', 5000);
				resetOverlay();
			},
			'mute:changed': (event) => {
				const data = event.payload || {};
				if (data.muted) {
					addNotification('麦克风已静音', 'info');
					if (get(recordingOverlay).isRecording) {
						addNotification('录音被静音强制停止', 'warning', 4000);
						resetOverlay('muted');
					}
				} else {
					addNotification('麦克风已取消静音', 'info');
				}
			},
			'tray:status_changed': (event) => {
				const data = event.payload || {};
				if (data.status === 'muted' && get(recordingOverlay).isRecording) {
					resetOverlay('muted');
				}
			},
			'hotkey:conflict': (event) => {
				const data = event.payload || {};
				addNotification(
					`Hotkey conflict: ${data.binding} - ${data.error}`,
					'error',
					5000,
				);
			},
			'hotkey:rebind': (event) => {
				const data = event.payload || {};
				if (data.new_binding) {
					hotkeyBinding = data.new_binding;
				}
			},
			'task:created': (event) => {
				const data = event.payload;
				const title = data.title || data.task_id;
				if (notifyCfg?.task_created?.in_app !== false) {
					addNotification(`新任务: ${title}`, 'info', 4000);
				}
				busyTasks = new Set(busyTasks).add(data.task_id);
				updateModelState('waiting', { idleTimeoutMs: 5000 });
			},
			'task:completed': (event) => {
				const data = event.payload;
				const title = data.title || data.task_id;
				if (notifyCfg?.task_completed?.in_app !== false) {
					addNotification(`任务已完成: ${title}`, 'success');
				}
				updateModelState('ready');
			},
			'task:deleted': (event) => {
				// delete_task / clear_history remove tasks without any terminal
				// `task:updated` (the task no longer exists), so release their
				// ids from the busy set here — otherwise the chip would stay on
				// "等待输出" for a task that is gone. `task_id: null` means all
				// tasks were removed (clear_history).
				const data = event.payload || {};
				if (data.task_id) {
					busyTasks = new Set([...busyTasks].filter((t) => t !== data.task_id));
				} else {
					busyTasks = new Set();
				}
				if (busyTasks.size === 0) {
					clearModelStateTimer();
					updateModelState('ready');
				}
			},
			'task:error': (event) => {
				const data = event.payload;
				const errMsg = data.error || data.task_id;
				if (notifyCfg?.task_error?.in_app !== false) {
					addNotification(`任务出错: ${errMsg}`, 'error', 5000);
				}
				clearModelStateTimer();
				updateModelState('ready');
			},
			'task:updated': (event) => {
				const data = event.payload;
				const title = data.title || data.task_id;
				const tid = data.task_id;
				if (data.status === 'running' || data.status === 'pending') {
					// The backend flips Pending -> Running in memory without
					// re-emitting, so treat both as busy. Any other status
					// transition below removes the task from the busy set.
					if (tid) busyTasks = new Set(busyTasks).add(tid);
				}
				if (data.status === 'paused') {
					if (tid) busyTasks = new Set([...busyTasks].filter((t) => t !== tid));
					if (notifyCfg?.task_paused?.in_app !== false) {
						addNotification(`任务已暂停: ${title || '未知'}`, 'warning', 3000);
					}
					clearModelStateTimer();
					updateModelState('ready');
				}
				if (data.status === 'pending') {
					if (notifyCfg?.task_resumed?.in_app !== false) {
						addNotification(`任务已恢复: ${title || '未知'}`, 'info', 3000);
					}
					updateModelState('waiting', { idleTimeoutMs: 5000 });
				}
				if (data.status === 'completed') {
					if (tid) busyTasks = new Set([...busyTasks].filter((t) => t !== tid));
					clearModelStateTimer();
					updateModelState('ready');
				}
				if (data.status === 'error') {
					if (tid) busyTasks = new Set([...busyTasks].filter((t) => t !== tid));
					clearModelStateTimer();
					updateModelState('ready');
				}
			},
			'mcp:status_change': (event) => {
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
			},
			'skills:status_change': () => {
				// Skill list refresh is notified by the tools page refresh button.
			},
			'agent:balanced_model': (event) => {
				const data = event.payload;
				const activeId = get(activeTaskIdStore);
				if (data.task_id && activeId && data.task_id !== activeId) return;
				updateModelState('balanced_model');
				addNotification(`Balanced Model: ${data.reason}`, 'warning');
			},
			'notification:show': (event) => {
				const data = event.payload || {};
				const title = data.title || 'Haven';
				const body = data.body || '新通知';
				// When the title is the default "Haven", showing "Haven: msg" is
				// redundant — the toast itself already lives in the app.
				addNotification(title === 'Haven' ? body : `${title}: ${body}`, 'info', 5000);
			},
			// Activity lifecycle (background jobs + reminders). Registered
			// globally (not on the chat page) so activities stay tracked while
			// the user visits other tabs. Job payloads carry `job_id`;
			// reminder payloads carry `id` — the store normalizes both.
			'activity:created': (event) => {
				upsertActivity(event.payload || {});
			},
			// Job attached to a task (payload has job_id) or a reminder was
			// cancelled (payload only has id).
			'activity:updated': (event) => {
				const p = event.payload || {};
				if (p.job_id) {
					upsertActivity(p);
				} else {
					removeActivity(p.id);
				}
			},
			// Live output preview while a job runs (bounded tail).
			'activity:output': (event) => {
				upsertActivity(event.payload || {});
			},
			'activity:finished': (event) => {
				const p = event.payload || {};
				if (p.job_id) {
					upsertActivity(p);
					// A background job finishing is only worth a toast when the
					// user is not already watching its owning task (the result
					// also lands in the task's conversation).
					if (p.status === 'completed' || p.status === 'failed') {
						const activeId = get(activeTaskIdStore);
						if (!p.task_id || p.task_id !== activeId) {
							const label = p.status === 'completed' ? '完成' : '失败';
							addNotification(
								`后台作业${label}: ${p.job_id || ''}`,
								p.status === 'completed' ? 'success' : 'error',
								4000,
							);
						}
					}
				} else {
					// Reminder fired: drop from the pending list. The toast is
					// surfaced by the agent's `notification:show` (the fired
					// consumer always notifies).
					removeActivity(p.id);
				}
			},
		}, { tag: '+layout' });
		eventRegistrations = registrations;
		await registrations.ready;

		// Hydrate the activity registry for activities started before this
		// mount (events only cover activities spawned after the listeners
		// above; fired/cancelled while the UI was away are already gone).
		refreshActivities();

		probeLlmConnection();
		scheduleLlmProbe();
		window.addEventListener('click', handleWindowClick);
	});

	onDestroy(() => {
		stopTimer();
		if (processingTimer) clearTimeout(processingTimer);
		if (llmProbeTimer) clearTimeout(llmProbeTimer);
		clearModelStateTimer();
		eventRegistrations?.dispose();
		if (typeof window !== 'undefined') {
			window.removeEventListener('click', handleWindowClick);
		}
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
			<div class="status-switch">
				<button
					class="status-chip status-chip-btn"
					onclick={() => (jobMenuOpen = !jobMenuOpen)}
					title={
						runningJobCount > 0 || pendingReminders.length > 0
							? `活动${runningJobCount > 0 ? `（${runningJobCount} 个作业运行中）` : ''}${pendingReminders.length > 0 ? `· 提醒（${pendingReminders.length} 条）` : ''}`
							: '活动：后台作业与提醒'
					}
					aria-label="活动：后台作业与提醒"
					type="button"
				>
					{#if overlay.isRecording}
						<StatusDot color="error" animate={true} />
						<span class="status-text recording-text">录音中</span>
					{:else if overlay.processing}
						<StatusDot color="warning" animate={true} />
						<span class="status-text">转写中</span>
					{:else if modelState === 'streaming'}
						<StatusDot color="primary" animate={true} />
						<span class="status-text">输出中</span>
					{:else if modelState === 'tool'}
						<StatusDot color="tertiary" animate={true} />
						<span class="status-text">工具调用</span>
					{:else if modelState === 'balanced_model'}
						<StatusDot color="error" animate={true} />
						<span class="status-text">备用模型</span>
					{:else if modelState === 'waiting' || taskBusy}
						<StatusDot color="warning" animate={true} />
						<span class="status-text"
							>{taskBusy ? `${busyTasks.size} 个任务运行中` : '等待输出'}</span
						>
					{:else if llmConnected === false}
						<StatusDot color="outline" />
						<span class="status-text">已断开</span>
					{:else}
						<StatusDot color="success" />
						<span class="status-text">就绪</span>
					{/if}
					{#if runningJobCount > 0 || pendingReminders.length > 0}
						<span class="status-badge">{runningJobCount + pendingReminders.length}</span>
					{/if}
				</button>
				{#if jobMenuOpen}
					<div class="status-job-menu job-menu">
						<div class="job-menu-tabs">
							<button
								class="job-menu-tab"
								class:active={activityTab === 'all'}
								onclick={() => (activityTab = 'all')}
								type="button"
								>全部</button
							>
							<button
								class="job-menu-tab"
								class:active={activityTab === 'job'}
								onclick={() => (activityTab = 'job')}
								type="button"
								>后台作业</button
							>
							<button
								class="job-menu-tab"
								class:active={activityTab === 'reminder'}
								onclick={() => (activityTab = 'reminder')}
								type="button"
								>提醒</button
							>
							<button
								class="job-menu-tab"
								class:active={activityTab === 'history'}
								onclick={() => {
									activityTab = 'history';
									historyLoaded = false;
								}}
								type="button"
								>历史</button
							>
						</div>
						{#if activityTab !== 'reminder'}
							<div class="job-menu-title">后台作业</div>
							{#if jobEntries.length === 0}
								<div class="job-menu-empty">暂无后台作业</div>
							{:else}
								{#each jobEntries as job}
									<div class="job-item" class:job-item-running={job.status === 'running'}>
										<span class="job-dot" style="color: {jobStatusColor(job.status)}"
											>&#9679;</span
										>
										<div class="job-item-main">
											<div class="job-item-top">
												<span class="job-id">{job.id}</span>
												<span
													class="job-item-status"
													class:running={job.status === 'running'}
													>{jobStatusLabel(job.status)}</span
												>
											</div>
											<div class="job-item-sub">
												<span class="job-task">{taskTitleFor(job)}</span>
												<span class="job-duration">{jobDuration(job)}</span>
											</div>
											{#if job.status === 'running' && job.output}
												<div class="job-output">{job.output}</div>
											{/if}
											{#if job.status === 'failed' && job.error_reason}
												<div class="job-error">{job.error_reason}</div>
											{/if}
										</div>
										{#if job.status === 'running'}
											<button
												class="job-cancel"
												onclick={() => handleCancelActivity(job.id, 'job')}
												title="停止后台作业"
												aria-label="停止后台作业"
												type="button"
												>&#x2715;</button
											>
										{/if}
									</div>
								{/each}
							{/if}
						{/if}
						{#if activityTab !== 'job'}
							<div class="job-menu-title reminder-menu-title">提醒</div>
							{#if pendingReminders.length === 0}
								<div class="job-menu-empty">暂无提醒</div>
							{:else}
								{#each pendingReminders as r}
									<div class="job-item reminder-item">
										<span class="reminder-dot">&#9200;</span>
										<div class="job-item-main">
											<div class="job-item-top">
												<span class="reminder-title-text">{r.title || r.body}</span>
												<span class="job-item-status"
													>{r.mode === 'continue' ? '续接任务' : '执行工具'}</span
												>
											</div>
											<div class="job-item-sub">
												<span class="reminder-body">{r.body}</span>
												<span class="job-duration">{reminderCountdown(r.due_at)}</span>
											</div>
										</div>
										<button
											class="job-cancel"
											onclick={() => handleCancelActivity(r.id, 'reminder')}
											title="取消提醒"
											aria-label="取消提醒"
											type="button"
											>&#x2715;</button
										>
									</div>
								{/each}
							{/if}
						{/if}
						{#if activityTab === 'history'}
							<div class="job-menu-title reminder-menu-title">已触发</div>
							{#if activityHistory.length === 0}
								<div class="job-menu-empty">暂无历史记录</div>
							{:else}
								{#each activityHistory as h}
									<div class="job-item reminder-item">
										<span class="reminder-dot">&#9989;</span>
										<div class="job-item-main">
											<div class="job-item-top">
												<span class="reminder-title-text">{h.title || h.body || '提醒'}</span>
												<span class="job-item-status"
													>{h.mode === 'continue' ? '已续接任务' : '已执行'}</span
												>
											</div>
											<div class="job-item-sub">
												<span class="reminder-body">{h.body}</span>
												<span class="job-duration">{formatHistoryTime(h)}</span>
											</div>
										</div>
										<button
											class="job-cancel"
											onclick={() => handleDeleteHistory(h.id)}
											title="删除历史记录"
											aria-label="删除历史记录"
											type="button"
											>&#x2715;</button
										>
									</div>
								{/each}
							{/if}
						{/if}
					</div>
				{/if}
			</div>
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
	.status-switch {
		position: relative;
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
		border: none;
		cursor: pointer;
		font-family: inherit;
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.status-chip-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.status-badge {
		min-width: 16px;
		height: 16px;
		padding: 0 4px;
		border-radius: 999px;
		background: var(--md-sys-color-tertiary, #9c6bff);
		color: #fff;
		font-size: 10px;
		font-weight: 700;
		line-height: 16px;
		text-align: center;
		font-variant-numeric: tabular-nums;
	}
	.recording-text {
		color: var(--md-sys-color-error);
	}
	.job-menu {
		position: absolute;
		right: 0;
		top: calc(100% + 8px);
		z-index: 1000;
		min-width: 280px;
		max-width: 360px;
		max-height: 360px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.job-menu-tabs {
		display: flex;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-xs) var(--md-sys-space-xs) 0;
	}
	.job-menu-tab {
		flex: 1;
		font-size: 11px;
		padding: 4px var(--md-sys-space-sm);
		border: none;
		border-radius: var(--md-sys-shape-extra-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		cursor: pointer;
	}
	.job-menu-tab.active {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-weight: 600;
	}
	.job-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.job-menu-empty {
		padding: var(--md-sys-space-lg) var(--md-sys-space-md);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		text-align: center;
	}
	.job-item {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		opacity: 0.85;
	}
	.job-item-running {
		opacity: 1;
		background: var(--md-sys-color-surface-container);
	}
	.job-dot {
		font-size: 10px;
		margin-top: 3px;
		flex-shrink: 0;
	}
	.job-item-main {
		flex: 1;
		min-width: 0;
	}
	.job-item-top {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.job-id {
		font-size: 11px;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface);
		font-weight: 600;
	}
	.job-item-status {
		flex-shrink: 0;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
	}
	.job-item-status.running {
		color: #44cc44;
	}
	.job-item-sub {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-top: 2px;
		min-width: 0;
	}
	.job-task {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.job-duration {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
		flex-shrink: 0;
		font-family: var(--md-sys-typescale-mono);
	}
	.job-error {
		margin-top: 4px;
		font-size: 10px;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-error);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.job-output {
		margin-top: 4px;
		max-height: 64px;
		overflow: hidden;
		font-size: 10px;
		font-family: var(--md-sys-typescale-mono);
		line-height: 1.45;
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		word-break: break-all;
		opacity: 0.9;
	}
	.reminder-menu-title {
		margin-top: var(--md-sys-space-xs);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 0;
	}
	.reminder-dot {
		font-size: 12px;
		line-height: 1;
		margin-top: 2px;
		flex-shrink: 0;
	}
	.reminder-title-text {
		font-size: 12px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.reminder-body {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.job-cancel {
		flex-shrink: 0;
		width: 22px;
		height: 22px;
		border: none;
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-error);
		font-size: 12px;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.job-cancel:hover {
		background: var(--md-sys-color-error-container);
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