<script>
	import '../app.css';
	import {
		addNotification,
		recordingOverlay,
		activeSessionIdStore,
		modelStateStore,
		updateModelState,
		clearModelStateTimer,
		upsertAction,
		removeAction,
		refreshActions,
		actionStore,
		sessionStore,
		cancelAction,
		refreshActionHistory,
		deleteAction,
		formatMessageTime,
		resumeTargetStore,
	} from '$lib/stores.ts';
	import { submitVoiceTranscript } from '$lib/voiceSubmit.ts';
	import { themeStore } from '$lib/themeStore.ts';
	import { invoke, isTauri } from '$lib/tauri.ts';
	import logger from '$lib/logger.ts';
	import { installGlobalErrorHandlers, reportError } from '$lib/errorHandling.ts';
	import {
		actionEventListeners,
		agentEventListeners,
		appEventListeners,
		recordingEventListeners,
		registerListeners,
		sessionEventListeners,
	} from '$lib/events.ts';
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { syncStore } from '$lib/syncStore.ts';
	import { isBusyStatus, isPausedStatus } from '$lib/sessionStatus.ts';
	import { confirmLeaveSettingsIfNeeded } from '$lib/settingsGuard.ts';
	import { actionStatusLabel } from '$lib/taskTerminology.ts';

	import AppShell from '$lib/AppShell.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import LoadingState from '$lib/LoadingState.svelte';
	import WorkspaceStatus from '$lib/WorkspaceStatus.svelte';

	let { children } = $props();

	// Secondary workspaces are intentionally loaded after the chat shell is
	// interactive. Their views contain the largest forms, lists and tool cards;
	// keeping them out of the initial module graph makes the first conversation
	// paint independent of settings/tools/memory/task-center code.
	/** @type {Record<string, () => Promise<{ default: any }>>} */
	const LAZY_VIEW_LOADERS = {
		tasks: () => import('$lib/TaskCenter.svelte'),
		tools: () => import('$lib/views/ToolsView.svelte'),
		memory: () => import('$lib/views/MemoryView.svelte'),
		settings: () => import('$lib/views/SettingsView.svelte'),
	};
	/** @type {Record<string, any>} */
	let lazyViewComponents = $state({});
	/** @type {Record<string, 'loading'|'ready'|'error'|undefined>} */
	let lazyViewStates = $state({});

	/** @param {string} id */
	function loadTabView(id) {
		if (id === 'chat' || lazyViewComponents[id] || lazyViewStates[id] === 'loading') return;
		const loader = LAZY_VIEW_LOADERS[id];
		if (!loader) return;
		lazyViewStates[id] = 'loading';
		void loader()
			.then((module) => {
				lazyViewComponents[id] = module.default;
				lazyViewStates[id] = 'ready';
			})
			.catch((/** @type {unknown} */ error) => {
				lazyViewStates[id] = 'error';
				logger.warn('+layout', `load ${id} view error`, error);
			});
	}

	/** @param {string} id */
	function retryTabView(id) {
		lazyViewStates[id] = undefined;
		loadTabView(id);
	}

	// Top-level tab state. Views stay MOUNTED once first activated (keep-alive)
	// instead of being destroyed/re-created on every switch, so switching is
	// instant and rapid tab clicks never tear down a view that is being
	// revisited. The URL is kept in sync via `?tab=<id>` (replaceState), which
	// also makes direct deep links (/tools etc.) restore the right tab.
	// Legacy `history` / `/history` map to `memory` (X6 memory center).
	const TAB_IDS = ['chat', 'tasks', 'tools', 'memory', 'settings'];
	function initialTabFromUrl() {
		if (typeof window === 'undefined') return 'chat';
		const url = get(page).url;
		const tabParam = url.searchParams.get('tab');
		if (tabParam === 'history') return 'memory';
		if (tabParam && TAB_IDS.includes(tabParam)) return tabParam;
		const path = url.pathname;
		if (path === '/tools') return 'tools';
		if (String(path) === '/tasks') return 'tasks';
		if (path === '/memory' || path === '/history') return 'memory';
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
		tasks: initialTab === 'tasks',
		memory: initialTab === 'memory',
		settings: initialTab === 'settings',
	});
	// While a leave-settings confirm is in flight, ignore URL-driven tab
	// sync so a concurrent `?tab=` change cannot race past the dialog.
	let leaveSettingsPending = false;
	// While applyTab has set activeTab but goto has not yet updated $page.url,
	// ignore URL sync. Otherwise the effect sees activeTab=settings with a
	// stale ?tab=chat and mis-fires the leave-settings bounce (settings
	// appears unopenable; other tabs self-heal when the URL catches up).
	let applyingTab = false;
	// Keep-alive views remain mounted, so this separate state replays the short
	// entry motion each time a workspace is shown without resetting its data.
	let enteringTab = /** @type {string | null} */ ($state(null));

	/** @param {string} id */
	function activateTab(id) {
		activeTab = id;
		visited[id] = true;
		enteringTab = id;
		loadTabView(id);
	}

	/** @param {string} id @param {Event} event */
	function finishTabEntry(id, event) {
		if (event.target !== event.currentTarget || enteringTab !== id) return;
		enteringTab = null;
	}

	/** @param {string} id */
	function applyTab(id) {
		applyingTab = true;
		activateTab(id);
		void goto('/?tab=' + id, { replaceState: true }).finally(() => {
			applyingTab = false;
		});
	}

	/** @param {string} id */
	async function switchTab(id) {
		if (id === activeTab || leaveSettingsPending) return;
		if (activeTab === 'settings' && id !== 'settings') {
			leaveSettingsPending = true;
			try {
				const ok = await confirmLeaveSettingsIfNeeded();
				if (!ok) return;
			} finally {
				leaveSettingsPending = false;
			}
		}
		applyTab(id);
	}

	/** @param {string} sessionId */
	function openTaskSession(sessionId) {
		if (!sessionId) return;
		resumeTargetStore.set({ sessionId, wasError: false });
		activeSessionIdStore.set(sessionId);
		switchTab('chat');
	}

	function startNewSessionFromTasks() {
		activeSessionIdStore.set(null);
		switchTab('chat');
	}
	let theme = $state(themeStore.currentTheme);
	$effect(() => syncStore(themeStore, (v) => (theme = v.theme)));

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
	// Cold-start gate: false until MCP/skills/audio prewarm finish (or the
	// get_bootstrap_status probe says ready). Keeps the chip on 加载中 so the
	// UI can paint before deferred backend work completes.
	let bootstrapReady = $state(false);
	// Whether ANY session is busy (pending/running). The model-state events only
	// fire while chunks flow; a session whose LLM call is stuck (idle timeout,
	// empty-response retries, provider hang) emits nothing, and the 5s idle
	// timer would flip the chip back to "就绪" mid-hang. sessionBusy keeps the
	// chip truthful: driven by session:created / session:updated transitions, which
	// the backend emits on every status change (pending/running on submission,
	// paused/completed/error on termination). Tracked per session id so a
	// parallel session completing does not clear the busy state of another.
	let busySessions = $state(new Set());
	/** @type {Map<string, string>} */
	let lastSessionStatus = new Map();
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
		// Browser / SSR / tests have no backend — skip without WARN spam or
		// treating the missing IPC as a real disconnect.
		if (!isTauri()) return;
		llmProbeInFlight = true;
		try {
			const status = await invoke('check_llm_connection');
			llmConnected =
				status === 'ready' || status === 'disconnected' || status === 'unconfigured'
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

	$effect(() => syncStore(recordingOverlay, (v) => (overlay = v)));

	$effect(() => {
		if (typeof window === 'undefined') return;
		const url = $page.url;
		const path = url.pathname;
		if (path !== '/') {
			// Legacy direct deep link (/tools, /memory|/history, /settings):
			// normalize to the keep-alive URL scheme so the root route (chat)
			// stays mounted. `/history` and `?tab=history` map to memory (X6).
			const t =
				path === '/tools'
					? 'tools'
					: String(path) === '/tasks'
						? 'tasks'
						: path === '/memory' || path === '/history'
							? 'memory'
							: path === '/settings'
								? 'settings'
								: 'chat';
			goto('/?tab=' + t, { replaceState: true });
			return;
		}
		const rawTab = url.searchParams.get('tab');
		const tabParam = rawTab === 'history' ? 'memory' : rawTab;
		const t = TAB_IDS.includes(tabParam || '') ? tabParam || 'chat' : 'chat';
		if (t === activeTab) {
			visited[t] = true;
			loadTabView(t);
			return;
		}
		if (leaveSettingsPending || applyingTab) return;
		if (activeTab === 'settings' && t !== 'settings') {
			// External / deep-link navigation away from dirty settings: revert
			// the URL and run the same leave prompt as a tab click.
			goto('/?tab=settings', { replaceState: true });
			leaveSettingsPending = true;
			confirmLeaveSettingsIfNeeded()
				.then((ok) => {
					if (ok) applyTab(t);
				})
				.finally(() => {
					leaveSettingsPending = false;
				});
			return;
		}
		activateTab(t);
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
			reportError(e, { context: '+layout', message: '停止录音失败', log: false });
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
	// derive from one store keyed by the normalized action id.
	let activities = $state({});
	$effect(() => syncStore(actionStore, (v) => (activities = v)));
	const actionEntries = $derived(Object.values(activities));
	const backgroundActionEntries = $derived(
		actionEntries
			.filter((a) => a.kind === 'background')
			.sort((a, b) => String(b.startedAt || '').localeCompare(String(a.startedAt || ''))),
	);
	const pendingScheduledActions = $derived(
		actionEntries
			.filter((a) => a.kind === 'scheduled')
			.sort((a, b) => String(a.dueAt || '').localeCompare(String(b.dueAt || ''))),
	);
	const runningBackgroundActions = $derived(
		backgroundActionEntries.filter((action) => action.status === 'running'),
	);
	const runningActionCount = $derived(runningBackgroundActions.length);
	// Active chat is plain-paused while its own background action(s) still run
	// — titlebar should say "等待后台任务" so it does not look idle/ready.
	let activeSessionId = $state(/** @type {string | null} */ (null));
	$effect(() => syncStore(activeSessionIdStore, (v) => (activeSessionId = v)));
	const awaitingBackgroundActive = $derived.by(() => {
		if (!activeSessionId) return false;
		const st = sessions.find((t) => t.id === activeSessionId)?.status;
		if (st !== 'paused') return false;
		return runningBackgroundActions.some((a) => a.sessionId === activeSessionId);
	});

	// Completed-task history (terminal background rows + fired scheduled rows),
	// fetched whenever the panel opens so it reflects the persisted table.
	let actionHistory = /** @type {Array<any>} */ ($state([]));
	$effect(() => {
		if (activeTab !== 'tasks') return;
		// Fetch a wider window so the task center can show cross-session history.
		refreshActionHistory(null, 200).then((rows) => {
			if (rows) actionHistory = rows;
		});
	});
	// Terminal background / fired scheduled rows across all sessions. The task
	// center is the global operational view; session filtering belongs in its
	// search controls rather than at the data boundary.
	const completedActions = $derived(
		actionHistory.filter((h) => {
			if (h.kind === 'scheduled') return true;
			return !!h.status && h.status !== 'running';
		}),
	);

	// Session titles for background-action rows; mirrored from the chat page's
	// loadSessions().
	let sessions = /** @type {Array<any>} */ ($state([]));
	$effect(() => syncStore(sessionStore, (v) => (sessions = v)));

	// Foreground running sessions: active (non-terminal) conversations.
	const runningSessions = $derived(
		sessions.filter((t) => isBusyStatus(t.status) || isPausedStatus(t.status)),
	);

	// While the panel is open, re-render once a second so countdowns tick.
	let countdownTick = $state(0);
	$effect(() => {
		if (activeTab !== 'tasks') return;
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
			reportError(e, { context: '+layout', message: '删除历史记录失败', log: false });
		}
	}

	/** @param {any} action */
	function sessionTitleFor(action) {
		if (!action.sessionId) return '';
		const t = sessions.find((x) => x.id === action.sessionId);
		return t?.title || t?.input || action.sessionId;
	}

	/** @param {any} action */
	function actionDuration(action) {
		const start = new Date(action.startedAt).getTime();
		if (isNaN(start)) return '';
		const end =
			action.status === 'running'
				? Date.now()
				: new Date(action.finishedAt || action.startedAt).getTime();
		if (isNaN(end)) return '';
		const secs = Math.floor((end - start) / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	/** @param {string} actionId @param {'background'|'scheduled'} [kind] */
	async function handleCancelAction(actionId, kind = 'background') {
		try {
			const ok = await cancelAction(actionId, kind);
			if (!ok) {
				// Backend already dropped/finished the action (completed, failed,
				// or session-end cancel) without a matching live-board update —
				// clear the ghost row only. Do NOT finalize tool cards as
				// cancelled: that would overwrite a successful terminal payload
				// and block a later action:finished repair.
				removeAction(actionId);
				if (activeTab === 'tasks') {
					refreshActionHistory(null, 200).then((rows) => {
						if (rows) actionHistory = rows;
					});
				}
				addNotification(
					kind === 'scheduled' ? '定时任务已触发或不存在' : '后台任务已结束，无需停止',
					'warning',
					2500,
				);
			}
		} catch (e) {
			reportError(e, {
				context: '+layout',
				message: `${kind === 'scheduled' ? '取消定时任务' : '停止后台任务'}失败`,
				log: false,
			});
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
		const ts = h.finishedAt || h.startedAt || h.dueAt;
		if (!ts) return '';
		const d = new Date(ts);
		if (isNaN(d.getTime())) return '';
		return formatMessageTime(d);
	}

	let eventRegistrations = /** @type {{ ready: Promise<void>; dispose: () => void } | null} */ (
		null
	);
	let removeGlobalErrorHandlers = () => {};

	onMount(async () => {
		removeGlobalErrorHandlers = installGlobalErrorHandlers();
		// Keep the static shell above the live DOM until it has had a paint pass.
		// This avoids exposing a partially hydrated layout for one frame, while
		// the shell's structure and tokens keep the visual handoff quiet.
		const bootShell = document.getElementById('haven-boot');
		if (bootShell) {
			requestAnimationFrame(() => {
				requestAnimationFrame(() => bootShell.remove());
			});
		}
		loadTabView(activeTab);

		// Load notify config in background — don't block
		// listener registration. Skip outside Tauri (browser / SSR preview).
		if (isTauri()) {
			invoke('get_settings')
				.then((settings) => {
					if (settings?.notification) {
						notifyCfg = { ...notifyCfg, ...settings.notification };
					}
				})
				.catch((e) => {
					logger.warn('+layout', 'get_settings error', e);
				});
		}

		const registrations = registerListeners(
			{
				...appEventListeners({
					'app:bootstrap': (event) => {
						const status = event?.payload?.status;
						if (status === 'ready') {
							bootstrapReady = true;
							probeLlmConnection();
						} else if (status === 'loading') {
							bootstrapReady = false;
						}
					},
				}),
				...recordingEventListeners({
					'recording:started': (event) => {
						const data = event.payload;
						setOverlay({
							visible: true,
							isRecording: true,
							processing: false,
							sessionId: data.sessionId || null,
							startedAt: Date.now(),
							reason: null,
							vadState: 'silent',
						});
						startTimer();
					},
					'recording:stopped': (event) => {
						const data = event.payload;
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
						const data = event.payload;
						if (get(recordingOverlay).isRecording) {
							setOverlay({ vadState: data.state || 'silent' });
						}
					},
					'recording:error': (event) => {
						const data = event.payload;
						addNotification(
							data.error || '录音错误，请检查麦克风/STT 配置',
							'error',
							5000,
						);
						resetOverlay();
					},
					'transcription:started': (event) => {
						addNotification('正在转写录音…', 'info', 2000);
						setOverlay({ processing: true });
					},
					'transcription:result': (event) => {
						const data = event.payload;
						const text = (data.text || '').trim();
						if (text) {
							// Same path as a typed message (see `submitVoiceTranscript`):
							// appends the voice message, submits with the current
							// `activeSessionId`, and migrates the message into the session if
							// the backend created a fresh one.
							submitVoiceTranscript(text).catch((e) => {
								reportError(e, {
									context: '+layout',
									message: '语音提交失败',
									log: false,
								});
							});
						} else {
							// 转写为空：静音或过短的录音没有产出任何内容，必须给用户
							// 明确反馈，否则看起来像"点了没反应"。
							const durationMs = data.durationMs || 0;
							if (durationMs > 0 && durationMs < 1000) {
								addNotification('录音时间太短，请再试一次', 'warning', 3000);
							} else {
								addNotification('未检测到语音，请再试一次', 'error', 4000);
							}
						}
						resetOverlay();
					},
					'transcription:error': (event) => {
						const data = event.payload;
						addNotification(
							data.error || '转写失败，请检查 STT 服务配置',
							'error',
							5000,
						);
						resetOverlay();
					},
				}),
				...appEventListeners({
					'mute:changed': (event) => {
						const data = event.payload;
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
						const data = event.payload;
						addNotification(`热键冲突: ${data.binding} - ${data.error}`, 'error', 5000);
					},
				}),
				...sessionEventListeners({
					'session:created': (event) => {
						const data = event.payload;
						const title = data.title || data.sessionId;
						if (notifyCfg?.session_created?.in_app !== false) {
							addNotification(`新会话: ${title}`, 'info', 4000);
						}
						lastSessionStatus.set(data.sessionId, data.status);
						busySessions = new Set(busySessions).add(data.sessionId);
						updateModelState('waiting', { idleTimeoutMs: 5000 });
					},
					'session:completed': (event) => {
						const data = event.payload;
						const title = data.title || data.sessionId;
						if (notifyCfg?.session_completed?.in_app !== false) {
							addNotification(`会话已完成: ${title}`, 'success');
						}
						updateModelState('ready');
					},
					'session:deleted': (event) => {
						// delete_session / clear_history remove sessions without any terminal
						// `session:updated` (the session no longer exists), so release their
						// ids from the busy set here — otherwise the chip would stay on
						// "等待响应" for a session that is gone. `sessionId: null` means all
						// sessions were removed (clear_history).
						const data = event.payload;
						if (data.sessionId) {
							busySessions = new Set(
								[...busySessions].filter((t) => t !== data.sessionId),
							);
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
						const errMsg = data.error || data.sessionId;
						if (notifyCfg?.session_error?.in_app !== false) {
							addNotification(`会话出错: ${errMsg}`, 'error', 5000);
						}
						clearModelStateTimer();
						updateModelState('ready');
					},
					'session:updated': (event) => {
						const data = event.payload;
						const title = data.title || data.sessionId;
						const tid = data.sessionId;
						const prev = tid ? lastSessionStatus.get(tid) : undefined;
						if (isBusyStatus(data.status)) {
							// pending = queued; running = claimed (handler now emits
							// running on claim). Both keep the session in the busy set.
							if (tid) busySessions = new Set(busySessions).add(tid);
						}
						if (isPausedStatus(data.status)) {
							if (tid)
								busySessions = new Set([...busySessions].filter((t) => t !== tid));
							if (notifyCfg?.session_paused?.in_app !== false) {
								addNotification(`会话已暂停: ${title || '未知'}`, 'warning', 3000);
							}
							clearModelStateTimer();
							updateModelState('ready');
						}
						if (data.status === 'pending') {
							// Only paused/error → pending is a real resume; Running→Pending
							// (ask answered in-turn) must not toast.
							if (
								(isPausedStatus(prev) || prev === 'error') &&
								notifyCfg?.session_resumed?.in_app !== false
							) {
								addNotification(`会话已恢复: ${title || '未知'}`, 'info', 3000);
							}
							updateModelState('waiting', { idleTimeoutMs: 5000 });
						}
						if (data.status === 'completed') {
							if (tid)
								busySessions = new Set([...busySessions].filter((t) => t !== tid));
							clearModelStateTimer();
							updateModelState('ready');
						}
						if (data.status === 'error') {
							if (tid)
								busySessions = new Set([...busySessions].filter((t) => t !== tid));
							clearModelStateTimer();
							updateModelState('ready');
						}
						if (tid && data.status) {
							lastSessionStatus.set(tid, data.status);
						}
					},
				}),
				...appEventListeners({
					'mcp:status_change': (event) => {
						const data = event.payload;
						const name = data.name || '';
						const status = data.status;
						// Skip Connecting toasts — cold start connects every server and
						// the status chip already shows 加载中. ToolsView still refreshes.
						if (status === 'Connected') {
							if (bootstrapReady) {
								addNotification(`MCP 已连接: ${name}`, 'success', 3000);
							}
						} else if (status === 'Disconnected') {
							addNotification(`MCP 已断开: ${name}`, 'warning', 4000);
						} else if (status && typeof status === 'object' && 'Offline' in status) {
							const err = status.Offline.error || '';
							addNotification(
								`MCP 离线: ${name}${err ? ` - ${err}` : ''}`,
								'error',
								5000,
							);
						}
					},
					'skills:status_change': () => {
						// Skill list refresh is notified by the tools page refresh button.
					},
				}),
				...agentEventListeners({
					'agent:stream_stalled': (event) => {
						// Provider stream went silent while the step is still in flight
						// (first-chunk wait or mid-step gap). Show the factual waiting
						// state — not a guessed "slow" label. Cleared by the next chunk
						// (streaming) or a terminal session event (ready/error).
						const data = event.payload;
						const activeId = get(activeSessionIdStore);
						if (data.sessionId && activeId && data.sessionId !== activeId) return;
						updateModelState('stalled');
					},
				}),
				// Router rebuilt (settings saved / model switched): re-probe LLM
				// connectivity immediately instead of waiting for the next
				// backoff-scheduled probe (which can be up to 120s away during a
				// failure streak).
				...appEventListeners({
					'llm:config_changed': () => {
						refreshLlmConnection();
					},
				}),
				...agentEventListeners({
					'notification:show': (event) => {
						const data = event.payload;
						const title = data.title || 'Haven';
						const body = data.body || '新通知';
						// When the title is the default "Haven", showing "Haven: msg" is
						// redundant — the toast itself already lives in the app.
						addNotification(
							title === 'Haven' ? body : `${title}: ${body}`,
							'info',
							5000,
						);
					},
				}),
				...actionEventListeners({
					// Action lifecycle is registered globally so tasks stay tracked while
					// the user visits other tabs. Both action kinds now share the named
					// task DTO and use camelCase after this boundary.
					'action:created': (event) => {
						upsertAction(event.payload);
					},
					'action:updated': (event) => {
						const p = event.payload;
						if (p.kind === 'background') {
							upsertAction(p);
						} else {
							removeAction(p.id);
						}
					},
					'action:output': (event) => {
						upsertAction(event.payload);
					},
					'action:finished': (event) => {
						const p = event.payload;
						if (p.kind === 'background') {
							upsertAction(p);
							// A background action finishing is only worth a toast when the
							// user is not already watching its owning session (the result
							// also lands in the session's conversation).
							if (p.status === 'completed' || p.status === 'failed') {
								const activeId = get(activeSessionIdStore);
								if (!p.sessionId || p.sessionId !== activeId) {
									const label = p.status === 'completed' ? '完成' : '失败';
									addNotification(
										`后台任务${label}: ${p.id}`,
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
				}),
			},
			{ tag: '+layout' },
		);
		eventRegistrations = registrations;
		// Attach listeners BEFORE probing bootstrap status so a ready event
		// that fires in the gap cannot be missed (probe-then-listen TOCTOU).
		await registrations.ready;

		if (isTauri()) {
			try {
				const status = await invoke('get_bootstrap_status');
				if (status === 'ready') {
					bootstrapReady = true;
					probeLlmConnection();
				}
			} catch (e) {
				logger.warn('+layout', 'get_bootstrap_status error', e);
				// Do not report readiness when the backend status probe failed. The
				// bootstrap event listener above can still transition us to ready;
				// otherwise the loading state remains honest instead of claiming the
				// backend is usable after an unknown failure.
				bootstrapReady = false;
			}
		} else {
			bootstrapReady = true;
		}

		// Hydrate the action registry for actions started before this mount
		// (events only cover actions spawned after the listeners above;
		// fired/cancelled while the UI was away are already gone).
		refreshActions();

		// The modelStateStore subscribe above fires synchronously on mount
		// (modelState is 'ready') and triggers the first probe; here we just
		// start the cadence for all subsequent probes (Tauri only).
		if (isTauri()) scheduleLlmProbe();
	});

	onDestroy(() => {
		removeGlobalErrorHandlers();
		stopTimer();
		if (processingTimer) clearTimeout(processingTimer);
		if (llmProbeTimer) clearTimeout(llmProbeTimer);
		clearModelStateTimer();
		eventRegistrations?.dispose();
	});

	const tabs = [
		{ id: 'chat', label: '对话' },
		{ id: 'tasks', label: '任务' },
		{ id: 'tools', label: '工具' },
		{ id: 'memory', label: '记忆' },
		{ id: 'settings', label: '设置' },
	];
</script>

<AppShell
	{activeTab}
	{tabs}
	{theme}
	onToggleTheme={toggleTheme}
	onNavigate={switchTab}
	{overlay}
	{duration}
	onCancelRecording={cancelRecording}
>
	{#snippet status()}
		<WorkspaceStatus
			{overlay}
			{modelState}
			{busySessions}
			{bootstrapReady}
			{llmConnected}
			{awaitingBackgroundActive}
			{runningActionCount}
			{pendingScheduledActions}
			onOpenTasks={() => switchTab('tasks')}
		/>
	{/snippet}
	{#snippet content()}
		{#each tabs as tab (tab.id)}
			<div
				class="tab-panel"
				id="workspace-tabpanel-{tab.id}"
				hidden={activeTab !== tab.id}
				role="tabpanel"
				aria-labelledby="workspace-tab-{tab.id}"
				aria-hidden={activeTab !== tab.id}
			>
				{#if visited[tab.id]}
					{@const TabComponent = lazyViewComponents[tab.id]}
					{#if tab.id === 'chat'}
						<div
							class="page-shell tab-view-surface"
							class:tab-view-surface--entering={enteringTab === tab.id}
							onanimationend={(event) => finishTabEntry(tab.id, event)}
						>
							{@render children()}
						</div>
					{:else if tab.id === 'tools'}
						<div class="page-shell">
							{#if lazyViewComponents.tools}
								<div
									class="tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
								>
									<TabComponent />
								</div>
							{:else if lazyViewStates.tools === 'error'}
								<div
									class="lazy-view-placeholder tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
									role="alert"
								>
									<span>工具页面暂时无法加载</span>
									<MaterialButton
										variant="outlined"
										label="重试"
										onclick={() => retryTabView('tools')}
									/>
								</div>
							{:else}
								<LoadingState label="正在加载工具…" detail="正在准备工具列表" />
							{/if}
						</div>
					{:else if tab.id === 'tasks'}
						<div class="page-shell">
							{#if lazyViewComponents.tasks}
								<div
									class="tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
								>
									<TabComponent
										{runningSessions}
										{runningBackgroundActions}
										{pendingScheduledActions}
										{completedActions}
										{actionStatusLabel}
										{sessionTitleFor}
										{actionDuration}
										{scheduledActionCountdown}
										{formatHistoryTime}
										onOpenSession={openTaskSession}
										onCancel={handleCancelAction}
										onDeleteHistory={handleDeleteHistory}
										onNewSession={startNewSessionFromTasks}
									/>
								</div>
							{:else if lazyViewStates.tasks === 'error'}
								<div
									class="lazy-view-placeholder tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
									role="alert"
								>
									<span>任务暂时无法加载</span>
									<MaterialButton
										variant="outlined"
										label="重试"
										onclick={() => retryTabView('tasks')}
									/>
								</div>
							{:else}
								<LoadingState label="正在加载任务…" detail="正在准备任务列表" />
							{/if}
						</div>
					{:else if tab.id === 'memory'}
						<div class="page-shell">
							{#if lazyViewComponents.memory}
								<div
									class="tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
								>
									<TabComponent onNewSession={startNewSessionFromTasks} />
								</div>
							{:else if lazyViewStates.memory === 'error'}
								<div
									class="lazy-view-placeholder tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
									role="alert"
								>
									<span>记忆页面暂时无法加载</span>
									<MaterialButton
										variant="outlined"
										label="重试"
										onclick={() => retryTabView('memory')}
									/>
								</div>
							{:else}
								<LoadingState label="正在加载记忆…" detail="正在准备记忆中心" />
							{/if}
						</div>
					{:else if tab.id === 'settings'}
						<div class="page-shell">
							{#if lazyViewComponents.settings}
								<div
									class="tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
								>
									<TabComponent />
								</div>
							{:else if lazyViewStates.settings === 'error'}
								<div
									class="lazy-view-placeholder tab-view-surface"
									class:tab-view-surface--entering={enteringTab === tab.id}
									onanimationend={(event) => finishTabEntry(tab.id, event)}
									role="alert"
								>
									<span>设置页面暂时无法加载</span>
									<MaterialButton
										variant="outlined"
										label="重试"
										onclick={() => retryTabView('settings')}
									/>
								</div>
							{:else}
								<LoadingState label="正在加载设置…" detail="正在准备设置页面" />
							{/if}
						</div>
					{/if}
				{/if}
			</div>
		{/each}
	{/snippet}
</AppShell>
