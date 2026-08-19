<script>
	import '../app.css';
	import { addNotification, recordingOverlay, activeSessionIdStore, modelStateStore, updateModelState, clearModelStateTimer, upsertAction, removeAction, refreshActions, actionStore, sessionStore, cancelAction, refreshActionHistory, deleteAction, formatMessageTime } from '$lib/stores.ts';
	import { submitVoiceTranscript } from '$lib/voiceSubmit.ts';
	import { themeStore } from '$lib/themeStore.ts';
	import { invoke } from '$lib/tauri.ts';
	import logger from '$lib/logger.ts';
	import { registerListeners } from '$lib/events.ts';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { syncStore } from '$lib/syncStore.ts';

	import RecordingIndicator from '$lib/RecordingIndicator.svelte';
	import Logo from '$lib/Logo.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import NotificationToast from '$lib/NotificationToast.svelte';
	import ToolsView from '$lib/views/ToolsView.svelte';
	import HistoryView from '$lib/views/HistoryView.svelte';
	import AgentMessagingView from '$lib/views/AgentMessagingView.svelte';
	import SettingsView from '$lib/views/SettingsView.svelte';

	let { children } = $props();

	// Top-level tab state. Views stay MOUNTED once first activated (keep-alive)
	// instead of being destroyed/re-created on every switch, so switching is
	// instant and rapid tab clicks never tear down a view that is being
	// revisited. The URL is kept in sync via `?tab=<id>` (replaceState), which
	// also makes direct deep links (/tools etc.) restore the right tab.
	const TAB_IDS = ['chat', 'tools', 'history', 'agents', 'settings'];
	function initialTabFromUrl() {
		if (typeof window === 'undefined') return 'chat';
		const url = get(page).url;
		const tabParam = url.searchParams.get('tab');
		if (tabParam && TAB_IDS.includes(tabParam)) return tabParam;
		const path = url.pathname;
		if (path === '/tools') return 'tools';
		if (path === '/history') return 'history';
		if (path === '/agents') return 'agents';
		if (path === '/settings') return 'settings';
		return 'chat';
	}
	const initialTab = initialTabFromUrl();
	let activeTab = $state(initialTab);
	// `visited` gates the first mount of each view so the app boots with only
	// the chat view; once a tab has been opened its view is kept alive.
	/** @type {Record<string, boolean>} */
	let visited = $state({
		chat: true,
		tools: initialTab === 'tools',
		history: initialTab === 'history',
		agents: initialTab === 'agents',
		settings: initialTab === 'settings',
	});

	/** @param {string} id */
	function switchTab(id) {
		if (id === activeTab) return;
		activeTab = id;
		visited[id] = true;
		goto('/?tab=' + id, { replaceState: true });
	}
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
	let durationTimer = /** @type {ReturnType<typeof setInterval> | null} */ (null);
	let processingTimer = /** @type {ReturnType<typeof setTimeout> | null} */ (null);
	let modelState = $state('ready'); // synced from modelStateStore on mount
	// Whether ANY session is busy (pending/running). The model-state events only
	// fire while chunks flow; a session whose LLM call is stuck (idle timeout,
	// empty-response retries, provider hang) emits nothing, and the 5s idle
	// timer would flip the chip back to "就绪" mid-hang. sessionBusy keeps the
	// chip truthful: driven by session:created / session:updated transitions, which
	// the backend emits on every status change (pending/running on submission,
	// paused/completed/error on termination). Tracked per session id so a
	// parallel session completing does not clear the busy state of another.
	let busySessions = $state(new Set());
	const sessionBusy = $derived(busySessions.size > 0);
	// Probe state is declared BEFORE the subscribe below: the store's
	// `subscribe` fires synchronously (SSR/mount) with the current value, and
	// `probeLlmConnection` reads these bindings without awaiting first, so
	// they must be initialized already.
	// `llmConnected` is a three-way status from the backend's
	// `check_llm_connection`: 'ready' | 'disconnected' | 'unconfigured'.
	// `null` = probe in-flight / never completed (show 检测中, never a false
	// 就绪).
	let llmConnected = /** @type {string | null} */ ($state(null));
	let llmProbeTimer = /** @type {ReturnType<typeof setTimeout> | undefined} */ (undefined);
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
			const status = await invoke('check_llm_connection');
			llmConnected = status === 'ready' || status === 'disconnected' || status === 'unconfigured'
				? status
				: 'disconnected';
			llmProbeFailureStreak = status === 'ready' ? 0 : Math.min(llmProbeFailureStreak + 1, 4);
		} catch (e) {
			logger.warn('+layout', 'check_llm_connection error', e);
			llmConnected = 'disconnected';
			llmProbeFailureStreak = Math.min(llmProbeFailureStreak + 1, 4);
		} finally {
			llmProbeInFlight = false;
		}
	}

	// Adaptive schedule: back off on consecutive failures (15s → 30s → 60s →
	// 120s cap), reset to 15s after a successful probe. A dead endpoint no
	// longer causes an unconditional multi-second network request every 15s.
	// Unlike the old version, the next interval is computed AFTER the probe
	// resolves (the streak it just updated), so the backoff actually tightens
	// on the first failure instead of lagging one probe behind.
	function nextLlmProbeInterval() {
		return llmProbeFailureStreak === 0
			? LLM_PROBE_INTERVAL_MS
			: Math.min(
					LLM_PROBE_INTERVAL_MS * 2 ** llmProbeFailureStreak,
					LLM_PROBE_MAX_INTERVAL_MS,
				);
	}
	function scheduleLlmProbe() {
		clearTimeout(llmProbeTimer);
		llmProbeTimer = setTimeout(async () => {
			await probeLlmConnection();
			scheduleLlmProbe();
		}, nextLlmProbeInterval());
	}
	// Force an immediate re-probe (config changed via settings / model switch):
	// reset the failure backoff, probe now, then resume from the base cadence.
	function refreshLlmConnection() {
		llmProbeFailureStreak = 0;
		probeLlmConnection();
		scheduleLlmProbe();
	}

	let notifyCfg = $state({
		session_created: { in_app: true },
		session_completed: { in_app: true },
		session_paused: { in_app: true },
		session_resumed: { in_app: true },
		session_error: { in_app: true },
	});

	// The configured recording hotkey binding (e.g. "Ctrl+Shift+Space"),
	// loaded from settings so the statusbar hint reflects the real value
	// instead of a hardcoded string. Updated live on `hotkey:rebind`.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	$effect(() => syncStore(recordingOverlay, (v) => (overlay = v)));

	$effect(() => {
		if (typeof window === 'undefined') return;
		const url = $page.url;
		const path = url.pathname;
		if (path !== '/') {
			// Legacy direct deep link (/tools, /history, /settings): normalize
			// to the keep-alive URL scheme so the root route (chat) stays mounted.
		const t =
			path === '/tools' ? 'tools' :
			path === '/history' ? 'history' :
			path === '/agents' ? 'agents' :
			path === '/settings' ? 'settings' : 'chat';
		goto('/?tab=' + t, { replaceState: true });
			return;
		}
		const tabParam = url.searchParams.get('tab');
		const t = TAB_IDS.includes(tabParam || '') ? tabParam || 'chat' : 'chat';
		activeTab = t;
		visited[t] = true;
	});

	/** @param {object} patch */
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
	/** @param {string | null} [reason] */
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

	// Action registry (background actions + scheduled actions) mirrored from
	// actionStore (kept live by the `action:*` listeners above). Background
	// actions sort newest-first; scheduled actions sort soonest-first; both
	// derive from one store keyed by the normalized action id. The status chip
	// in the titlebar opens a menu of these, replacing the old chat-toolbar
	// button.
	let actionMenuOpen = $state(false);
	let activities = $state({});
	$effect(() => syncStore(actionStore, (v) => (activities = v)));
	const actionEntries = $derived(Object.values(activities));
	const backgroundActionEntries = $derived(
		actionEntries
			.filter((a) => a.kind === 'background')
			.sort((a, b) =>
				String(b.started_at || '').localeCompare(String(a.started_at || '')),
			),
	);
	const pendingScheduledActions = $derived(
		actionEntries
			.filter((a) => a.kind === 'scheduled')
			.sort((a, b) => String(a.due_at || '').localeCompare(String(b.due_at || ''))),
	);
	const runningBackgroundActions = $derived(
		backgroundActionEntries.filter((j) => j.status === 'running'),
	);
	const runningActionCount = $derived(runningBackgroundActions.length);

	// Completed-task history (terminal background rows + fired scheduled rows),
	// fetched whenever the panel opens so it reflects the persisted table.
	let actionHistory = /** @type {Array<any>} */ ($state([]));
	$effect(() => {
		if (!actionMenuOpen) return;
		refreshActionHistory(null, 50).then((rows) => (actionHistory = rows));
	});
	// Terminal background rows (not running) and fired scheduled rows only —
	// pending scheduled actions stay in their own section, running background
	// actions in the running section, so the completed list never duplicates.
	const completedActions = $derived(
		actionHistory.filter(
			(h) =>
				(h.kind === 'scheduled' && h.fired) ||
				(h.kind !== 'scheduled' && h.status && h.status !== 'running'),
		),
	);

	// Session titles for background-action rows; mirrored from the chat page's
	// loadSessions().
	let sessions = /** @type {Array<any>} */ ($state([]));
	$effect(() => syncStore(sessionStore, (v) => (sessions = v)));

	// Foreground running tasks: active (non-terminal) conversations.
	const runningSessions = $derived(
		sessions.filter(
			(t) => t.status === 'running' || t.status === 'pending' || t.status === 'paused',
		),
	);

	// While the panel is open, re-render once a second so countdowns tick.
	let countdownTick = $state(0);
	$effect(() => {
		if (!actionMenuOpen) return;
		const t = setInterval(() => (countdownTick += 1), 1000);
		return () => clearInterval(t);
	});

	/** @param {string} id */
	async function handleDeleteHistory(id) {
		try {
			await deleteAction(id);
			actionHistory = actionHistory.filter((h) => h.id !== id);
			addNotification('已删除历史记录', 'success', 2000);
		} catch (e) {
			addNotification(`删除历史记录失败: ${e}`, 'error', 3000);
		}
	}

	/** @param {string} status */
	function actionStatusLabel(status) {
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

	/** @param {string} status */
	function actionStatusColor(status) {
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

	/** @param {any} action */
	function sessionTitleFor(action) {
		if (!action.session_id) return '';
		const t = sessions.find((x) => x.id === action.session_id);
		return t?.title || t?.input || action.session_id;
	}

	/** @param {any} action */
	function actionDuration(action) {
		const start = new Date(action.started_at).getTime();
		if (isNaN(start)) return '';
		const end =
			action.status === 'running'
				? Date.now()
				: new Date(action.finished_at || action.started_at).getTime();
		if (isNaN(end)) return '';
		const secs = Math.floor((end - start) / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	/** @param {string} actionId @param {string} [kind] */
	async function handleCancelAction(actionId, kind = 'background') {
		try {
			const ok = await cancelAction(actionId, kind);
			if (!ok) {
				addNotification(
					kind === 'scheduled' ? '定时任务已触发或不存在' : '后台任务已结束，无需停止',
					'warning',
					2500,
				);
			}
		} catch (e) {
			addNotification(
				`${kind === 'scheduled' ? '取消定时任务' : '停止后台任务'}失败: ${e}`,
				'error',
				3000,
			);
		}
	}

	/** @param {string} dueAt */
	function scheduledActionCountdown(dueAt) {
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

	/** @param {any} h */
	function formatHistoryTime(h) {
		const ts = h.finished_at || h.started_at || h.due_at;
		if (!ts) return '';
		const d = new Date(ts);
		if (isNaN(d.getTime())) return '';
		return formatMessageTime(d);
	}

	/** @param {MouseEvent} e */
	function handleWindowClick(e) {
		if (actionMenuOpen) {
			const menu = document.querySelector('.status-action-menu');
			const chip = document.querySelector('.status-chip-btn');
			if (menu && chip && !menu.contains(/** @type {Node} */ (e.target)) && !chip.contains(/** @type {Node} */ (e.target))) {
				actionMenuOpen = false;
			}
		}
	}

	let eventRegistrations = /** @type {{ ready: Promise<void>; dispose: () => void } | null} */ (null);

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
			'transcription:started': (event) => {
				addNotification('正在转写录音…', 'info', 2000);
				setOverlay({ processing: true });
			},
			'transcription:result': (event) => {
				const data = event.payload || {};
				const text = (data.text || '').trim();
				if (text) {
					// Same path as a typed message (see `submitVoiceTranscript`):
					// appends the voice message, submits with the current
					// `activeSessionId`, and migrates the message into the session if
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
			'session:created': (event) => {
				const data = event.payload;
				const title = data.title || data.session_id;
				if (notifyCfg?.session_created?.in_app !== false) {
					addNotification(`新会话: ${title}`, 'info', 4000);
				}
				busySessions = new Set(busySessions).add(data.session_id);
				updateModelState('waiting', { idleTimeoutMs: 5000 });
			},
			'session:completed': (event) => {
				const data = event.payload;
				const title = data.title || data.session_id;
				if (notifyCfg?.session_completed?.in_app !== false) {
					addNotification(`会话已完成: ${title}`, 'success');
				}
				updateModelState('ready');
			},
			'session:deleted': (event) => {
				// delete_session / clear_history remove sessions without any terminal
				// `session:updated` (the session no longer exists), so release their
				// ids from the busy set here — otherwise the chip would stay on
				// "等待输出" for a session that is gone. `session_id: null` means all
				// sessions were removed (clear_history).
				const data = event.payload || {};
				if (data.session_id) {
					busySessions = new Set([...busySessions].filter((t) => t !== data.session_id));
				} else {
					busySessions = new Set();
				}
				if (busySessions.size === 0) {
					clearModelStateTimer();
					updateModelState('ready');
				}
			},
			'session:error': (event) => {
				const data = event.payload;
				const errMsg = data.error || data.session_id;
				if (notifyCfg?.session_error?.in_app !== false) {
					addNotification(`会话出错: ${errMsg}`, 'error', 5000);
				}
				clearModelStateTimer();
				updateModelState('ready');
			},
			'session:updated': (event) => {
				const data = event.payload;
				const title = data.title || data.session_id;
				const tid = data.session_id;
				if (data.status === 'running' || data.status === 'pending') {
					// The backend flips Pending -> Running in memory without
					// re-emitting, so treat both as busy. Any other status
					// transition below removes the session from the busy set.
					if (tid) busySessions = new Set(busySessions).add(tid);
				}
				if (data.status === 'paused') {
					if (tid) busySessions = new Set([...busySessions].filter((t) => t !== tid));
					if (notifyCfg?.session_paused?.in_app !== false) {
						addNotification(`会话已暂停: ${title || '未知'}`, 'warning', 3000);
					}
					clearModelStateTimer();
					updateModelState('ready');
				}
				if (data.status === 'pending') {
					if (notifyCfg?.session_resumed?.in_app !== false) {
						addNotification(`会话已恢复: ${title || '未知'}`, 'info', 3000);
					}
					updateModelState('waiting', { idleTimeoutMs: 5000 });
				}
				if (data.status === 'completed') {
					if (tid) busySessions = new Set([...busySessions].filter((t) => t !== tid));
					clearModelStateTimer();
					updateModelState('ready');
				}
				if (data.status === 'error') {
					if (tid) busySessions = new Set([...busySessions].filter((t) => t !== tid));
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
				const activeId = get(activeSessionIdStore);
				if (data.session_id && activeId && data.session_id !== activeId) return;
				updateModelState('balanced_model');
				addNotification(`Balanced Model: ${data.reason}`, 'warning');
			},
			'agent:stream_stalled': (event) => {
				// The provider stream went silent (mid-step stall, retry wait,
				// slow first chunk). Surface "still generating" feedback so the
				// conversation never looks frozen; the next chunk (streaming)
				// or a terminal session event (ready/error) clears it.
				const data = event.payload || {};
				const activeId = get(activeSessionIdStore);
				if (data.session_id && activeId && data.session_id !== activeId) return;
				updateModelState('stalled');
			},
			// Router rebuilt (settings saved / model switched): re-probe LLM
			// connectivity immediately instead of waiting for the next
			// backoff-scheduled probe (which can be up to 120s away during a
			// failure streak).
			'llm:config_changed': () => {
				refreshLlmConnection();
			},
			'notification:show': (event) => {
				const data = event.payload || {};
				const title = data.title || 'Haven';
				const body = data.body || '新通知';
				// When the title is the default "Haven", showing "Haven: msg" is
				// redundant — the toast itself already lives in the app.
				addNotification(title === 'Haven' ? body : `${title}: ${body}`, 'info', 5000);
			},
			// Action lifecycle (background actions + scheduled actions). Registered
			// globally (not on the chat page) so actions stay tracked while
			// the user visits other tabs. Background-action payloads carry
			// `action_id`; scheduled-action payloads carry `id` — the store
			// normalizes both.
			'action:created': (event) => {
				upsertAction(event.payload || {});
			},
			// Background action attached to a session (payload has action_id) or
			// a scheduled action was cancelled (payload only has id).
			'action:updated': (event) => {
				const p = event.payload || {};
				if (p.action_id) {
					upsertAction(p);
				} else {
					removeAction(p.id);
				}
			},
			// Live output preview while a background action runs (bounded tail).
			'action:output': (event) => {
				upsertAction(event.payload || {});
			},
			'action:finished': (event) => {
				const p = event.payload || {};
				if (p.action_id) {
					upsertAction(p);
					// A background action finishing is only worth a toast when the
					// user is not already watching its owning session (the result
					// also lands in the session's conversation).
					if (p.status === 'completed' || p.status === 'failed') {
						const activeId = get(activeSessionIdStore);
						if (!p.session_id || p.session_id !== activeId) {
							const label = p.status === 'completed' ? '完成' : '失败';
							addNotification(
								`后台任务${label}: ${p.action_id || ''}`,
								p.status === 'completed' ? 'success' : 'error',
								4000,
							);
						}
					}
				} else {
					// Scheduled action fired: drop from the pending list. The
					// toast is surfaced by the agent's `notification:show` (the
					// fired consumer always notifies).
					removeAction(p.id);
				}
			},
		}, { tag: '+layout' });
		eventRegistrations = registrations;
		await registrations.ready;

		// Hydrate the action registry for actions started before this mount
		// (events only cover actions spawned after the listeners above;
		// fired/cancelled while the UI was away are already gone).
		refreshActions();

		// The modelStateStore subscribe above fires synchronously on mount
		// (modelState is 'ready') and triggers the first probe; here we just
		// start the cadence for all subsequent probes.
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
		{ id: 'chat', label: '对话' },
		{ id: 'tools', label: '工具' },
		{ id: 'history', label: '历史' },
		{ id: 'agents', label: '消息' },
		{ id: 'settings', label: '设置' },
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
					onclick={() => (actionMenuOpen = !actionMenuOpen)}
					title={
						runningActionCount > 0 || pendingScheduledActions.length > 0
							? `任务${runningActionCount > 0 ? `（${runningActionCount} 个后台任务运行中）` : ''}${pendingScheduledActions.length > 0 ? `· 定时任务（${pendingScheduledActions.length} 条）` : ''}`
							: '任务：后台任务与定时任务'
					}
					aria-label="任务：后台任务与定时任务"
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
					{:else if modelState === 'stalled'}
						<StatusDot color="warning" animate={true} />
						<span class="status-text">生成较慢</span>
					{:else if modelState === 'tool'}
						<StatusDot color="tertiary" animate={true} />
						<span class="status-text">工具调用</span>
					{:else if modelState === 'balanced_model'}
						<StatusDot color="error" animate={true} />
						<span class="status-text">备用模型</span>
					{:else if modelState === 'waiting' || sessionBusy}
						<StatusDot color="warning" animate={true} />
						<span class="status-text"
							>{sessionBusy ? `${busySessions.size} 个会话运行中` : '等待输出'}</span
						>
					{:else if llmConnected === 'unconfigured'}
						<StatusDot color="outline" />
						<span class="status-text">未配置</span>
					{:else if llmConnected === 'disconnected'}
						<StatusDot color="outline" />
						<span class="status-text">已断开</span>
					{:else if llmConnected === 'ready'}
						<StatusDot color="success" />
						<span class="status-text">就绪</span>
					{:else}
						<StatusDot color="outline" animate={true} />
						<span class="status-text">检测中</span>
					{/if}
					{#if runningActionCount > 0 || pendingScheduledActions.length > 0}
						<span class="status-badge">{runningActionCount + pendingScheduledActions.length}</span>
					{/if}
				</button>
				{#if actionMenuOpen}
					<div class="status-action-menu action-menu">
						<div class="action-menu-title">正在运行</div>
						{#if runningSessions.length === 0 && runningBackgroundActions.length === 0}
							<div class="action-menu-empty">暂无运行中的任务</div>
						{:else}
							{#if runningSessions.length > 0}
								<div class="action-menu-subtitle">前台（会话）</div>
								{#each runningSessions as t}
									<div class="action-item" class:action-item-running={t.status === 'running'}>
										<span
											class="action-dot"
											style="color: {t.status === 'running' ? '#44cc44' : '#e0a020'}"
											>&#9679;</span
										>
										<div class="action-item-main">
											<div class="action-item-top">
												<span class="action-id">{t.title || t.input}</span>
												<span
													class="action-item-status"
													class:running={t.status === 'running'}
													>{t.status === 'running'
														? '运行中'
														: t.status === 'paused'
															? '已暂停'
															: '等待中'}</span
												>
											</div>
											<div class="action-item-sub">
												<span class="action-session">{t.id}</span>
											</div>
										</div>
									</div>
								{/each}
							{/if}
							{#if runningBackgroundActions.length > 0}
								<div class="action-menu-subtitle">后台（任务）</div>
								{#each runningBackgroundActions as action}
									<div class="action-item action-item-running">
										<span
											class="action-dot"
											style="color: {actionStatusColor(action.status)}"
											>&#9679;</span
										>
										<div class="action-item-main">
											<div class="action-item-top">
												<span class="action-id">{action.id}</span>
												<span class="action-item-status running"
													>{actionStatusLabel(action.status)}</span
												>
											</div>
											<div class="action-item-sub">
												<span class="action-session">{sessionTitleFor(action)}</span>
												<span class="action-duration">{actionDuration(action)}</span>
											</div>
											{#if action.output}
												<div class="action-output">{action.output}</div>
											{/if}
										</div>
										<button
											class="action-cancel"
											onclick={() => handleCancelAction(action.id, 'background')}
											title="停止后台任务"
											aria-label="停止后台任务"
											type="button"
											>&#x2715;</button
										>
									</div>
								{/each}
							{/if}
						{/if}
						<div class="action-menu-title scheduled-menu-title">定时任务</div>
						{#if pendingScheduledActions.length === 0}
							<div class="action-menu-empty">暂无定时任务</div>
						{:else}
							{#each pendingScheduledActions as r}
								<div class="action-item scheduled-item">
									<span class="scheduled-dot">&#9200;</span>
									<div class="action-item-main">
										<div class="action-item-top">
											<span class="scheduled-title-text">{r.title || r.body}</span>
											<span class="action-item-status"
												>{r.mode === 'continue' ? '续接会话' : '执行工具'}</span
											>
										</div>
										<div class="action-item-sub">
											<span class="scheduled-body">{r.body}</span>
											<span class="action-duration">{scheduledActionCountdown(r.due_at)}</span>
										</div>
									</div>
									<button
										class="action-cancel"
										onclick={() => handleCancelAction(r.id, 'scheduled')}
										title="取消定时任务"
										aria-label="取消定时任务"
										type="button"
										>&#x2715;</button
									>
								</div>
							{/each}
						{/if}
						<div class="action-menu-title scheduled-menu-title">已完成</div>
						{#if completedActions.length === 0}
							<div class="action-menu-empty">暂无已完成的任务</div>
						{:else}
							{#each completedActions as h}
								<div class="action-item scheduled-item">
									<span class="scheduled-dot">&#9989;</span>
									<div class="action-item-main">
										<div class="action-item-top">
											<span class="scheduled-title-text"
												>{h.kind === 'scheduled'
													? h.title || h.body || '定时任务'
													: h.command || h.id}</span
											>
											<span class="action-item-status"
												>{h.kind === 'scheduled'
													? h.mode === 'continue'
														? '已续接会话'
														: '已执行'
													: actionStatusLabel(h.status)}</span
											>
										</div>
										<div class="action-item-sub">
											<span class="scheduled-body"
												>{h.kind === 'scheduled' ? h.body : h.output || h.error_reason || h.id}</span
											>
											<span class="action-duration">{formatHistoryTime(h)}</span>
										</div>
									</div>
									<button
										class="action-cancel"
										onclick={() => handleDeleteHistory(h.id)}
										title="删除记录"
										aria-label="删除记录"
										type="button"
										>&#x2715;</button
									>
								</div>
							{/each}
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
		{#each tabs as tab (tab.id)}
			<button
				type="button"
				class="md-tab"
				class:active={activeTab === tab.id}
				aria-selected={activeTab === tab.id}
				role="tab"
				onclick={() => switchTab(tab.id)}
			>
				<span class="tab-label">{tab.label}</span>
			</button>
		{/each}
	</nav>

	<main class="content" class:content--chat={activeTab === 'chat'}>
		{#each tabs as tab (tab.id)}
			<div
				class="tab-panel"
				hidden={activeTab !== tab.id}
				role="tabpanel"
				aria-hidden={activeTab !== tab.id}
			>
				{#if visited[tab.id]}
					{#if tab.id === 'chat'}
						<div class="page-shell">
							{@render children()}
						</div>
					{:else if tab.id === 'tools'}
						<div class="page-shell">
							<ToolsView />
						</div>
					{:else if tab.id === 'history'}
						<div class="page-shell">
							<HistoryView />
						</div>
					{:else if tab.id === 'agents'}
						<div class="page-shell">
							<AgentMessagingView />
						</div>
					{:else if tab.id === 'settings'}
						<div class="page-shell">
							<SettingsView />
						</div>
					{/if}
				{/if}
			</div>
		{/each}
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
	.action-menu {
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
	.action-menu-subtitle {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-md);
	}
	.action-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.action-menu-empty {
		padding: var(--md-sys-space-lg) var(--md-sys-space-md);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		text-align: center;
	}
	.action-item {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		opacity: 0.85;
	}
	.action-item-running {
		opacity: 1;
		background: var(--md-sys-color-surface-container);
	}
	.action-dot {
		font-size: 10px;
		margin-top: 3px;
		flex-shrink: 0;
	}
	.action-item-main {
		flex: 1;
		min-width: 0;
	}
	.action-item-top {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.action-id {
		font-size: 11px;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface);
		font-weight: 600;
	}
	.action-item-status {
		flex-shrink: 0;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
	}
	.action-item-status.running {
		color: #44cc44;
	}
	.action-item-sub {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-top: 2px;
		min-width: 0;
	}
	.action-session {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.action-duration {
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
		flex-shrink: 0;
		font-family: var(--md-sys-typescale-mono);
	}
	.action-output {
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
	.scheduled-menu-title {
		margin-top: var(--md-sys-space-xs);
		border-top: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 0;
	}
	.scheduled-dot {
		font-size: 12px;
		line-height: 1;
		margin-top: 2px;
		flex-shrink: 0;
	}
	.scheduled-title-text {
		font-size: 12px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.scheduled-body {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.action-cancel {
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
	.action-cancel:hover {
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
	/* Keep-alive tab panels: `hidden` hides the inactive views (display:none).
	 * The [hidden] rules must win over the flex layout used while the chat
	 * tab is active. */
	.tab-panel[hidden] {
		display: none;
	}
	.content--chat .tab-panel {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}
	.content--chat .tab-panel[hidden] {
		display: none;
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