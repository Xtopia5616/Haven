<script lang="ts">
	import '../app.css';
	import {
		reactExecutionPhaseStore,
		activeSessionStatusLabelStore,
		updateReactExecutionPhase,
	} from '$lib/sessionRuntimeStore.ts';
	import { recordingOverlayController } from '$lib/recordingOverlayController.ts';
	import { addNotification } from '$lib/notificationStore.ts';
	import {
		setToolRunCompletionNotificationChannels,
		shouldShowToolRunCompletionInApp,
	} from '$lib/toolRunCompletionNotificationSettings.ts';
	import {
		createToolRunCompletionNotificationGate,
		projectToolRunCompletionToast,
	} from '$lib/toolRunCompletionNotificationProjection.ts';
	import {
		upsertToolRun,
		removeToolRun,
		refreshToolRuns,
		toolRunStore,
		upsertSessionToolRun,
		cancelToolRun,
		finalizeBackgroundToolRunMessages,
	} from '$lib/toolRunStore.ts';
	import { sessionResumeTargetStore } from '$lib/sessionIntentStore.ts';
	import { appSessionReducer, createSessionSelectorStore } from '$lib/sessionReducer.ts';
	import { submitVoiceTranscript } from '$lib/voiceSubmit.ts';
	import { themeStore } from '$lib/themeStore.ts';
	import { invoke, isTauri } from '$lib/tauri.ts';
	import { formatError } from '$lib/formatError.ts';
	import { installGlobalErrorHandlers, reportError } from '$lib/errorHandling.ts';
	import {
		toolRunEventListeners,
		agentEventListeners,
		appEventListeners,
		recordingEventListeners,
		registerListeners,
		sessionEventListeners,
	} from '$lib/events.ts';
	import { onMount, onDestroy, tick } from 'svelte';
	import { get } from 'svelte/store';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { syncStore } from '$lib/syncStore.ts';
	import { isBusyStatus, isPausedStatus, sessionWaitingReason } from '$lib/sessionStatus.ts';
	import { confirmLeaveSettingsIfNeeded } from '$lib/settingsGuard.ts';
	import { loadSettings } from '$lib/settingsCommand.ts';
	import { toolRunStatusLabel } from '$lib/toolRunTerminology.ts';
	import { setToolManifests } from '$lib/toolManifest.ts';
	import { listBuiltinToolManifests } from '$lib/toolsCommands.ts';
	import { createChatInteractionEventHandlers } from '$lib/chatInteractionEventHandlers.ts';
	import {
		clearRequestedConfirmation,
		requestedConfirmationIdStore,
	} from '$lib/interactionPresentationStore.ts';
	import {
		formatLlmConnectionFailure,
		formatLlmConnectionRecovery,
		llmConnectionReasonText,
		normalizeLlmConnectionReport,
	} from '$lib/llmConnection.ts';
	import {
		BOOTSTRAP_PROBE_INTERVAL_MS,
		isBootstrapReady,
		nextBootstrapProbeInterval,
	} from '$lib/bootstrapStatus.ts';
	import type { RecordingOverlayState } from '$lib/recordingOverlayController.ts';
	import type { ReActExecutionPhase } from '$lib/sessionRuntimeStore.ts';
	import type { ToolRunKind, ToolRunPayload } from '$lib/contracts/toolRun.ts';
	import type { ConfirmationDecision } from '$lib/confirmationTypes.ts';
	import type { AgentNotificationPayload } from '$lib/contracts/agent.ts';
	import type { NotificationConfigInput } from '$lib/contracts/generatedCommands.ts';
	import { interactionOwnerToWire } from '$lib/contracts/app.ts';
	import type { LlmConnectionReportView } from '$lib/llmConnection.ts';
	import type { LlmConnectionStatus } from '$lib/contracts/generatedCommands.ts';

	import AppShell from '$lib/AppShell.svelte';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import LoadingState from '$lib/LoadingState.svelte';
	import WorkspaceSurface from '$lib/WorkspaceSurface.svelte';
	import WorkspaceStatus from '$lib/WorkspaceStatus.svelte';

	let { children } = $props();

	// Secondary workspaces are intentionally loaded after the chat shell is
	// interactive. Their views contain the largest forms, lists and tool cards;
	// keeping them out of the initial module graph makes the first chat
	// paint independent of settings/tools/memory code. ToolRunCenter is nested in
	// the history workspace and is loaded with MemoryView.
	type TabId = 'chat' | 'tools' | 'memory' | 'settings';
	type LazyViewId = Exclude<TabId, 'chat'>;
	type LazyViewComponents = {
		tools: (typeof import('$lib/views/ToolsView.svelte'))['default'];
		memory: (typeof import('$lib/views/MemoryView.svelte'))['default'];
		settings: (typeof import('$lib/views/SettingsView.svelte'))['default'];
	};
	type LazyViewState = 'loading' | 'ready' | 'error';

	const LAZY_VIEW_LOADERS: {
		[K in LazyViewId]: () => Promise<{ default: LazyViewComponents[K] }>;
	} = {
		tools: () => import('$lib/views/ToolsView.svelte'),
		memory: () => import('$lib/views/MemoryView.svelte'),
		settings: () => import('$lib/views/SettingsView.svelte'),
	};
	let lazyViewComponents = $state<Partial<LazyViewComponents>>({});
	let lazyViewStates = $state<Partial<Record<LazyViewId, LazyViewState>>>({});

	function isTabId(value: string | null): value is TabId {
		return (
			value !== null &&
			(['chat', 'tools', 'memory', 'settings'] as const).includes(value as TabId)
		);
	}
	function isLazyViewId(value: string): value is LazyViewId {
		return value === 'tools' || value === 'memory' || value === 'settings';
	}

	function loadTypedTabView<K extends LazyViewId>(
		id: K,
		loader: () => Promise<{ default: LazyViewComponents[K] }>,
	) {
		if (lazyViewComponents[id] || lazyViewStates[id] === 'loading') return;
		lazyViewStates[id] = 'loading';
		void loader()
			.then((module) => {
				lazyViewComponents[id] = module.default;
				lazyViewStates[id] = 'ready';
			})
			.catch((error: unknown) => {
				lazyViewStates[id] = 'error';
				reportError(error, {
					context: '+layout',
					message: `加载 ${id} 页面失败`,
					notify: false,
				});
			});
	}

	function loadTabView(value: string) {
		if (!isLazyViewId(value)) return;
		switch (value) {
			case 'tools':
				loadTypedTabView('tools', LAZY_VIEW_LOADERS.tools);
				break;
			case 'memory':
				loadTypedTabView('memory', LAZY_VIEW_LOADERS.memory);
				break;
			case 'settings':
				loadTypedTabView('settings', LAZY_VIEW_LOADERS.settings);
				break;
		}
	}

	function retryTabView(id: LazyViewId) {
		lazyViewStates[id] = undefined;
		loadTabView(id);
	}

	// Top-level tab state. Views stay MOUNTED once first activated (keep-alive)
	// instead of being destroyed/re-created on every switch, so switching is
	// instant and rapid tab clicks never tear down a view that is being
	// revisited. The URL is kept in sync via `?tab=<id>` (replaceState).
	const TAB_IDS: readonly TabId[] = ['chat', 'tools', 'memory', 'settings'];
	function initialTabFromUrl(): TabId {
		if (typeof window === 'undefined') return 'chat';
		const url = get(page).url;
		const tabParam = url.searchParams.get('tab');
		if (isTabId(tabParam)) return tabParam;
		return 'chat';
	}
	const initialTab = initialTabFromUrl();
	let activeTab = $state<TabId>(initialTab);
	// `visited` gates the first mount of each view so the app boots with only
	// the chat view; once a tab has been opened its view is kept alive.
	let visited = $state<Record<TabId, boolean>>({
		chat: true,
		tools: initialTab === 'tools',
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
	let enteringTab = $state<string | null>(null);
	// Keep the chat panel mounted through its exit motion before hiding it.
	let leavingTab = $state<TabId | null>(null);
	let tabActivationSequence = 0;
	const tabEntryAnimations = new WeakMap<Element, Animation>();

	function playEntryAnimation(element: HTMLElement, keyframes: Keyframe[]) {
		if (typeof element.animate !== 'function') return;
		tabEntryAnimations.get(element)?.cancel();
		const animation = element.animate(keyframes, {
			duration: 300,
			easing: 'cubic-bezier(0, 0, 0, 1)',
			fill: 'none',
		});
		tabEntryAnimations.set(element, animation);
		animation.onfinish = () => {
			if (tabEntryAnimations.get(element) === animation) tabEntryAnimations.delete(element);
		};
	}

	function playTabEntryAnimations(id: TabId) {
		const panel = document.getElementById(`workspace-tabpanel-${id}`);
		if (!panel) return;

		// Give the whole retained tab a fresh animation on every visit. This does
		// not depend on CSS animationend or on the one-shot child animations.
		playEntryAnimation(panel, [{ opacity: 0 }, { opacity: 1 }]);

		const animatedElements = panel.querySelectorAll<HTMLElement>(
			'.motion-surface-enter, .responsive-layout-panel',
		);
		for (const element of animatedElements) {
			if (element.getClientRects().length === 0) continue;
			const keyframes = element.matches('.responsive-layout-panel')
				? [
						{ opacity: 0, transform: 'translateX(-8px)' },
						{ opacity: 1, transform: 'translateX(0)' },
					]
				: [{ opacity: 0 }, { opacity: 1 }];
			playEntryAnimation(element, keyframes);
		}
	}

	function activateTab(id: TabId) {
		const activationSequence = ++tabActivationSequence;
		// Force the previous entry class to be removed even when an earlier
		// animation was interrupted before its animationend event could clear it.
		enteringTab = null;
		leavingTab = activeTab === 'chat' && id !== 'chat' ? 'chat' : null;
		activeTab = id;
		visited[id] = true;
		loadTabView(id);
		void tick().then(async () => {
			if (activeTab !== id || tabActivationSequence !== activationSequence) return;
			const panel = document.getElementById(`workspace-tabpanel-${id}`);
			if (panel) void panel.offsetWidth;
			enteringTab = id;
			await tick();
			if (activeTab !== id || tabActivationSequence !== activationSequence) return;
			playTabEntryAnimations(id);
		});
	}

	function finishTabEntry(id: TabId, event: AnimationEvent) {
		if (event.target !== event.currentTarget || enteringTab !== id) return;
		enteringTab = null;
	}

	function finishTabLeave(id: TabId, event: AnimationEvent) {
		if (event.target !== event.currentTarget || leavingTab !== id) return;
		leavingTab = null;
	}

	function applyTab(id: TabId, section = '') {
		applyingTab = true;
		activateTab(id);
		const params = new URLSearchParams({ tab: id });
		if (section) params.set('section', section);
		void goto('/?' + params.toString(), { replaceState: true }).finally(() => {
			applyingTab = false;
		});
	}

	async function switchTab(id: string, section = ''): Promise<boolean> {
		if (!isTabId(id)) return false;
		if ((id === activeTab && !section) || leaveSettingsPending) return false;
		if (activeTab === 'settings' && id !== 'settings') {
			leaveSettingsPending = true;
			try {
				const ok = await confirmLeaveSettingsIfNeeded();
				if (!ok) return false;
			} finally {
				leaveSettingsPending = false;
			}
		}
		applyTab(id, section);
		return true;
	}

	function openTaskSession(sessionId: string) {
		if (!sessionId) return;
		sessionResumeTargetStore.set({ sessionId, wasError: false });
		appSessionReducer.dispatch({ type: 'session/selected', sessionId });
		switchTab('chat');
	}

	function startNewSessionFromTasks() {
		const currentSessionId = appSessionReducer.snapshot().activeSessionId;
		if (currentSessionId)
			appSessionReducer.dispatch({
				type: 'session/memory-cleared',
				sessionId: currentSessionId,
			});
		appSessionReducer.dispatch({ type: 'session/cleared' });
		switchTab('chat');
	}
	let theme = $state(themeStore.currentTheme);
	$effect(() => syncStore(themeStore, (v) => (theme = v.theme)));

	let overlay = $state<RecordingOverlayState>(recordingOverlayController.snapshot());
	let duration = $state(recordingOverlayController.durationSeconds());
	let reactExecutionPhase = $state<ReActExecutionPhase>('idle'); // synced from reactExecutionPhaseStore on mount
	let activeSessionStatusLabel = $state('空闲');
	$effect(() =>
		syncStore(activeSessionStatusLabelStore, (value) => (activeSessionStatusLabel = value)),
	);
	// Runtime mode is intentionally separate from backend bootstrap state:
	// browser Vite preview has no Tauri backend at all, while a Tauri webview
	// can still be waiting for Rust startup.
	let runtime = $state<'unknown' | 'tauri' | 'browser'>('unknown');
	// Cold-start gate: false until MCP/skills/audio prewarm is ready. The event
	// is best-effort, so get_bootstrap_status is retried when startup races with
	// a reused Vite/Tauri development process.
	let bootstrapReady = $state(false);
	let bootstrapProbeTimer: ReturnType<typeof setTimeout> | undefined;
	let bootstrapProbeInFlight = false;
	let bootstrapProbeFailureStreak = 0;
	// Whether ANY session is busy (pending/running). Tracked per session id so a
	// parallel session completing does not clear the busy state of another.
	let busySessions = $state(new Set<string>());
	let lastSessionStatus = new Map<string, string>();
	function addBusySession(sessionId: string | undefined) {
		if (!sessionId || busySessions.has(sessionId)) return;
		busySessions = new Set(busySessions).add(sessionId);
	}
	function removeBusySession(sessionId: string | undefined) {
		if (!sessionId || !busySessions.has(sessionId)) return;
		const nextBusySessions = new Set(busySessions);
		nextBusySessions.delete(sessionId);
		busySessions = nextBusySessions;
	}
	function clearBusySessions() {
		if (busySessions.size > 0) busySessions = new Set();
	}
	const sessionBusy = $derived(busySessions.size > 0);
	// Probe state is declared BEFORE the subscribe below: the store's
	// `subscribe` fires synchronously (SSR/mount) with the current value, and
	// `probeLlmConnection` reads these bindings without awaiting first, so
	// they must be initialized already.
	// `llmConnected` is independent from ReAct execution. `null` means the first
	// probe has not completed or the configuration changed and is being checked.
	let llmConnected = $state<LlmConnectionStatus | null>(null);
	let llmConnectionReport = $state<LlmConnectionReportView | null>(null);
	let llmProbeTimer: ReturnType<typeof setTimeout> | undefined;
	let llmProbeInFlight = false;
	let llmProbeGeneration = 0;
	let llmProbeFailureStreak = 0;
	const LLM_PROBE_INTERVAL_MS = 15000;
	const LLM_PROBE_MAX_INTERVAL_MS = 120000;
	function markBootstrapReady() {
		if (bootstrapReady) return;
		bootstrapReady = true;
		clearTimeout(bootstrapProbeTimer);
		bootstrapProbeTimer = undefined;
		probeLlmConnection();
	}
	function scheduleBootstrapProbe() {
		clearTimeout(bootstrapProbeTimer);
		if (bootstrapReady || !isTauri()) return;
		const delay =
			bootstrapProbeFailureStreak === 0
				? BOOTSTRAP_PROBE_INTERVAL_MS
				: nextBootstrapProbeInterval(bootstrapProbeFailureStreak);
		bootstrapProbeTimer = setTimeout(() => {
			void probeBootstrapStatus();
		}, delay);
	}
	async function probeBootstrapStatus() {
		if (bootstrapReady || bootstrapProbeInFlight || !isTauri()) return;
		bootstrapProbeInFlight = true;
		try {
			const status = await invoke('get_bootstrap_status');
			if (isBootstrapReady(status)) {
				bootstrapProbeFailureStreak = 0;
				markBootstrapReady();
			} else {
				bootstrapProbeFailureStreak = 0;
			}
		} catch (e) {
			bootstrapProbeFailureStreak = Math.min(bootstrapProbeFailureStreak + 1, 4);
			reportError(e, {
				context: '+layout',
				message: '读取启动状态失败',
				notify: false,
			});
		} finally {
			bootstrapProbeInFlight = false;
			scheduleBootstrapProbe();
		}
	}
	const unsubscribeExecutionPhase = syncStore(reactExecutionPhaseStore, (v) => {
		reactExecutionPhase = v.phase;
		if (v.phase === 'idle') probeLlmConnection();
	});
	/** @param {unknown} value */
	function applyLlmConnectionReport(value: unknown) {
		const report = normalizeLlmConnectionReport(value);
		const previous = llmConnected;
		llmConnectionReport = report;
		llmConnected = report.status;
		llmProbeFailureStreak =
			report.status === 'ready' ? 0 : Math.min(llmProbeFailureStreak + 1, 4);

		// A probe runs repeatedly, so notify only when the user-visible state
		// changes. The first disconnected result is still important at startup.
		if (report.status === 'disconnected' && previous !== 'disconnected') {
			addNotification(formatLlmConnectionFailure(report), 'error', 5000);
		} else if (report.status === 'ready' && previous === 'disconnected') {
			addNotification(formatLlmConnectionRecovery(report), 'success', 3000);
		} else if (report.status === 'unconfigured' && previous && previous !== 'unconfigured') {
			addNotification(
				'默认模型未配置，请到模型设置填写 Provider、模型和 API Key',
				'warning',
				4000,
			);
		}
	}
	async function probeLlmConnection() {
		if (reactExecutionPhase !== 'idle' || llmProbeInFlight) return;
		// Browser / SSR / tests have no backend — skip without WARN spam or
		// treating the missing IPC as a real disconnect.
		if (!isTauri()) return;
		llmProbeInFlight = true;
		const generation = llmProbeGeneration;
		try {
			const report = await invoke('check_llm_connection');
			if (generation === llmProbeGeneration) applyLlmConnectionReport(report);
		} catch (e) {
			if (generation !== llmProbeGeneration) return;
			reportError(e, { context: '+layout', message: '检查模型连接失败', log: false });
			llmConnectionReport = { status: 'disconnected', reason: 'unknown' };
			llmConnected = 'disconnected';
			llmProbeFailureStreak = Math.min(llmProbeFailureStreak + 1, 4);
		} finally {
			llmProbeInFlight = false;
			if (generation !== llmProbeGeneration) void probeLlmConnection();
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
		llmProbeGeneration += 1;
		llmProbeFailureStreak = 0;
		llmConnectionReport = null;
		llmConnected = null;
		probeLlmConnection();
		scheduleLlmProbe();
	}

	let notifyCfg = $state<NotificationConfigInput>({
		session_created: { in_app: true },
		session_completed: { in_app: true },
		session_paused: { in_app: true },
		session_resumed: { in_app: true },
		session_error: { in_app: true },
		permission_requested: { in_app: true },
	});

	function showAgentNotification(data: AgentNotificationPayload) {
		if (data.notificationKind === 'tool_run_completion') {
			if (!shouldShowToolRunCompletionInApp()) return;
			const toast = projectToolRunCompletionToast(
				data,
				appSessionReducer.snapshot().activeSessionId,
			);
			if (toast) addNotification(toast.message, toast.type, toast.durationMs);
			return;
		}
		const title = data.title || 'Haven';
		const body = data.body || '新通知';
		// When the title is the default "Haven", showing "Haven: msg" is
		// redundant — the toast itself already lives in the app.
		addNotification(title === 'Haven' ? body : `${title}: ${body}`, 'info', 5000);
	}

	const toolRunCompletionNotificationGate =
		createToolRunCompletionNotificationGate(showAgentNotification);

	$effect(() => syncStore(recordingOverlayController.state, (v) => (overlay = v)));
	$effect(() => syncStore(recordingOverlayController.duration, (v) => (duration = v)));

	$effect(() => {
		if (typeof window === 'undefined') return;
		const url = $page.url;
		if (url.pathname !== '/') return;
		const rawTab = url.searchParams.get('tab');
		const tabParam = rawTab;
		const t: TabId = isTabId(tabParam) ? tabParam : 'chat';
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

	async function cancelRecording() {
		try {
			await recordingOverlayController.cancel();
		} catch (e) {
			reportError(e, { context: '+layout', message: '停止录音失败', log: false });
		}
	}

	function toggleTheme() {
		themeStore.toggle();
		theme = themeStore.currentTheme;
	}

	// ToolRun registry (background and scheduled ToolRuns) mirrored from
	// toolRunStore, kept live by the `tool_run:*` listeners above. Background
	// ToolRuns sort newest-first; scheduled ToolRuns sort soonest-first; both
	// share one store keyed by the normalized ToolRun id.
	let toolRuns = $state<Record<string, ToolRunPayload>>({});
	$effect(() => syncStore(toolRunStore, (value) => (toolRuns = value)));
	const toolRunEntries = $derived(Object.values(toolRuns));
	const backgroundToolRunEntries = $derived(
		toolRunEntries
			.filter((toolRun) => toolRun.kind === 'background')
			.sort((left, right) =>
				String(right.startedAt || '').localeCompare(String(left.startedAt || '')),
			),
	);
	const pendingScheduledToolRuns = $derived(
		toolRunEntries
			.filter((toolRun) => toolRun.kind === 'scheduled')
			.sort((left, right) =>
				String(left.dueAt || '').localeCompare(String(right.dueAt || '')),
			),
	);
	const runningBackgroundToolRuns = $derived(
		backgroundToolRunEntries.filter((toolRun) => toolRun.status === 'running'),
	);
	const runningBackgroundToolRunCount = $derived(runningBackgroundToolRuns.length);
	const sessionsStore = createSessionSelectorStore((state) => state.sessions);
	const activeSessionIdStore = createSessionSelectorStore((state) => state.activeSessionId);
	const interactionsStore = createSessionSelectorStore((state) => state.interactions);
	let sessions = $state(appSessionReducer.snapshot().sessions);
	let activeSessionId = $state(appSessionReducer.snapshot().activeSessionId);
	let interactionDict = $state(appSessionReducer.snapshot().interactions);
	$effect(() => syncStore(sessionsStore, (v) => (sessions = v)));
	$effect(() => syncStore(activeSessionIdStore, (v) => (activeSessionId = v)));
	$effect(() => syncStore(interactionsStore, (v) => (interactionDict = v)));
	// Session lifecycle events expose the derived pause reason directly. The
	// ToolRun registry remains available for the task panel and counts, but it
	// no longer determines why a paused session is waiting.
	// Active chat is paused while its own background ToolRun(s) still run —
	// the selected session supplies the titlebar's "等待任务" state.
	const awaitingBackgroundActive = $derived.by(() => {
		if (!activeSessionId) return false;
		const session = sessions.find((t) => t.id === activeSessionId);
		return sessionWaitingReason(session) === 'background_task';
	});

	const taskCenterVisible = $derived(
		activeTab === 'memory' && $page.url.searchParams.get('section') === 'tasks',
	);

	// Permission confirmations belong to the application shell, not the chat
	// page. The chat page is kept mounted but hidden when another workspace is
	// active, so rendering the dialog there made pending requests invisible.
	const pendingConfirmInteractions = $derived(
		Object.values(interactionDict).filter(
			(request) =>
				request.status === 'pending' &&
				(request.kind === 'confirm' || request.kind === 'scheduled_confirm'),
		),
	);
	let dismissedConfirmationIds = $state(new Set<string>());
	let requestedConfirmationId = $state<string | null>(null);
	$effect(() => syncStore(requestedConfirmationIdStore, (id) => (requestedConfirmationId = id)));
	const activeConfirmRequest = $derived.by(() => {
		if (requestedConfirmationId) {
			const requested = pendingConfirmInteractions.find(
				(request) => request.id === requestedConfirmationId,
			);
			if (requested) return requested;
		}
		return (
			pendingConfirmInteractions.find(
				(request) => !dismissedConfirmationIds.has(request.id),
			) ||
			pendingConfirmInteractions[0] ||
			null
		);
	});
	const activeConfirmOpen = $derived(
		!!activeConfirmRequest &&
			(requestedConfirmationId === activeConfirmRequest.id ||
				!dismissedConfirmationIds.has(activeConfirmRequest.id)),
	);
	function dismissConfirmation(id: string) {
		dismissedConfirmationIds = new Set(dismissedConfirmationIds).add(id);
		if (requestedConfirmationId === id) clearRequestedConfirmation();
	}
	const activeConfirmSessionTitle = $derived(
		activeConfirmRequest
			? activeConfirmRequest.owner.kind === 'app_command'
				? '当前操作'
				: activeConfirmRequest.sessionId
					? String(
							sessions.find(
								(session) => session.id === activeConfirmRequest.sessionId,
							)?.title || activeConfirmRequest.sessionId,
						)
					: '定时任务'
			: '',
	);
	const activeConfirmDeadlineAt = $derived(
		activeConfirmRequest
			? activeConfirmRequest.expiresAt
				? Date.parse(activeConfirmRequest.expiresAt)
				: Number.NaN
			: null,
	);
	const confirmationRequestsInFlight = new Set<string>();

	async function handleConfirm({
		stepId,
		approved,
		effect,
		scope,
		target,
	}: ConfirmationDecision) {
		const resolvedStep = stepId;
		if (!resolvedStep || confirmationRequestsInFlight.has(resolvedStep)) return false;
		const currentRequest = appSessionReducer.snapshot().interactions[resolvedStep];
		if (!currentRequest || currentRequest.status !== 'pending') return false;
		confirmationRequestsInFlight.add(resolvedStep);
		const resolvedEffect = effect || (approved ? 'allow' : 'deny');
		const resolvedScope = scope || 'once';
		const resolvedTarget = target || 'operation';
		/** @type {import('$lib/contracts/commands.ts').ResolveConfirmationRequest} */
		const confirmationRequest = {
			requestId: resolvedStep,
			owner: interactionOwnerToWire(currentRequest.owner),
			effect: resolvedEffect,
			scope: resolvedScope,
			target: resolvedTarget,
		};
		try {
			const resolution = await invoke('resolve_confirmation', confirmationRequest);
			appSessionReducer.dispatch({
				type: 'session/interaction-resolution-result',
				id: resolvedStep,
				result: resolution,
				response: { approved, effect: resolvedEffect, scope: resolvedScope },
			});
			if (resolution === 'expired')
				addNotification('确认已过期，操作未执行', 'warning', 4000);
			else if (resolution === 'stale') {
				addNotification('确认请求已失效或已处理，请查看会话结果', 'warning', 4000);
			} else if (approved && currentRequest?.owner.kind === 'app_command') {
				addNotification('权限已确认，操作正在执行', 'info', 4000);
			}
			return true;
		} catch (e) {
			reportError(e, { context: '+layout', message: '确认失败', log: false });
			// A rejected command remains retryable until the owner reports an
			// accepted terminal transition (or the request becomes stale).
			return false;
		} finally {
			confirmationRequestsInFlight.delete(resolvedStep);
		}
	}

	// While the panel is open, re-render once a second so countdowns tick.
	let countdownTick = $state(0);
	$effect(() => {
		if (!taskCenterVisible) return;
		void refreshToolRuns();
		const t = setInterval(() => (countdownTick += 1), 1000);
		const reconciliation = setInterval(() => void refreshToolRuns(), 5000);
		return () => {
			clearInterval(t);
			clearInterval(reconciliation);
		};
	});

	function sessionTitleFor(toolRun: Pick<ToolRunPayload, 'sessionId'>) {
		if (!toolRun.sessionId) return '';
		const t = sessions.find((x) => x.id === toolRun.sessionId);
		const title = t?.title || t?.input;
		return typeof title === 'string' ? title : toolRun.sessionId;
	}

	function toolRunDuration(toolRun: ToolRunPayload) {
		const start = new Date(toolRun.startedAt ?? '').getTime();
		if (isNaN(start)) return '';
		const end =
			toolRun.status === 'running'
				? Date.now()
				: new Date(toolRun.finishedAt || toolRun.startedAt || '').getTime();
		if (isNaN(end)) return '';
		const secs = Math.floor((end - start) / 1000);
		if (secs < 60) return `${secs}s`;
		const mins = Math.floor(secs / 60);
		return `${mins}m ${secs % 60}s`;
	}

	async function handleCancelToolRun(toolRunId: string, kind: ToolRunKind = 'background') {
		try {
			const ok = await cancelToolRun(toolRunId, kind);
			if (!ok) {
				// False also covers a durable cancellation failure. Reconcile before
				// changing the UI so a live waiting/running ToolRun is never presented
				// as cancelled merely because the request returned false.
				await refreshToolRuns();
				addNotification(
					kind === 'scheduled'
						? '取消定时任务未生效，已重新同步状态'
						: '后台任务未停止，已重新同步状态',
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

	function scheduledToolRunCountdown(dueAt?: string) {
		const due = new Date(dueAt ?? '').getTime();
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

	let eventRegistrations: { ready: Promise<void>; dispose: () => void } | null = null;
	let removeGlobalErrorHandlers: () => void = () => {};

	onMount(async () => {
		recordingOverlayController.resumeTimer();
		runtime = isTauri() ? 'tauri' : 'browser';
		if (isTauri()) {
			listBuiltinToolManifests()
				.then((result) => setToolManifests(result?.tools))
				.catch((error) =>
					reportError(error, {
						context: '+layout',
						message: '预热工具清单不可用',
						notify: false,
					}),
				);
		}
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
			loadSettings()
				.then((settings) => {
					if (settings?.notification) {
						notifyCfg = { ...notifyCfg, ...settings.notification };
						setToolRunCompletionNotificationChannels(
							settings.notification.tool_run_completed,
						);
					}
				})
				.catch((e) => {
					reportError(e, {
						context: '+layout',
						message: '读取通知设置失败',
						notify: false,
					});
				})
				.finally(() => toolRunCompletionNotificationGate.settingsLoaded());
		} else {
			toolRunCompletionNotificationGate.settingsLoaded();
		}

		const registrations = registerListeners(
			{
				...appEventListeners(
					createChatInteractionEventHandlers({
						dispatchSession: (action) => appSessionReducer.dispatch(action),
						onPendingPermission: () => {
							if (notifyCfg?.permission_requested?.in_app !== false) {
								addNotification('有一项操作等待权限确认', 'warning', 5000);
							}
						},
					}),
				),
				...appEventListeners({
					'app:bootstrap': (event) => {
						const status = event?.payload?.status;
						if (status === 'ready') {
							markBootstrapReady();
						} else if (status === 'loading') {
							bootstrapReady = false;
						}
					},
				}),
				...recordingEventListeners({
					'recording:started': (event) => {
						recordingOverlayController.onRecordingStarted(event.payload);
					},
					'recording:stopped': (event) => {
						recordingOverlayController.onRecordingStopped(event.payload);
					},
					'recording:vad_status': (event) => {
						recordingOverlayController.onVadStatus(event.payload);
					},
					'recording:error': (event) => {
						const data = event.payload;
						addNotification(
							data.error || '录音错误，请检查麦克风/STT 配置',
							'error',
							5000,
						);
						recordingOverlayController.onRecordingError(data);
					},
					'transcription:started': (event) => {
						addNotification('正在转写录音…', 'info', 2000);
						recordingOverlayController.onTranscriptionStarted(event.payload.sessionId);
					},
					'transcription:result': (event) => {
						const data = event.payload;
						// Overlay completion is session-scoped; transcript delivery below
						// still runs for late results from an older recording.
						recordingOverlayController.onTranscriptionFinished(data.sessionId);
						const text = (data.text || '').trim();
						if (text) {
							// Same path as a typed message (see `submitVoiceTranscript`):
							// appends the voice message, submits with the current
							// `activeSessionId`, and migrates the message into the session if
							// the backend created a fresh one.
							submitVoiceTranscript(text, data.sessionId).catch((e) => {
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
					},
					'transcription:error': (event) => {
						const data = event.payload;
						addNotification(
							data.error || '转写失败，请检查 STT 服务配置',
							'error',
							5000,
						);
						recordingOverlayController.onTranscriptionFinished(data.sessionId);
					},
				}),
				...appEventListeners({
					'mute:changed': (event) => {
						const data = event.payload;
						if (data.muted) {
							addNotification('麦克风已静音', 'info');
							if (recordingOverlayController.snapshot().isRecording) {
								addNotification('录音被静音强制停止', 'warning', 4000);
								recordingOverlayController.reset();
							}
						} else {
							addNotification('麦克风已取消静音', 'info');
						}
					},
					'tray:status_changed': (event) => {
						const data = event.payload || {};
						if (
							data.status === 'muted' &&
							recordingOverlayController.snapshot().isRecording
						) {
							recordingOverlayController.reset();
						}
					},
					'hotkey:conflict': (event) => {
						const data = event.payload;
						addNotification(`热键冲突: ${data.binding} - ${data.error}`, 'error', 5000);
					},
				}),
				...sessionEventListeners((event) => {
					const data = event.payload;
					switch (data.type) {
						case 'created': {
							const title = data.title || data.sessionId;
							if (notifyCfg?.session_created?.in_app !== false) {
								addNotification(`新会话: ${title}`, 'info', 4000);
							}
							lastSessionStatus.set(data.sessionId, data.status);
							addBusySession(data.sessionId);
							updateReactExecutionPhase(
								data.sessionId,
								data.status === 'running' ? 'requesting' : 'queued',
							);
							return;
						}
						case 'updated': {
							const { sessionId, status } = data;
							const title = data.title || sessionId;
							const previousStatus = lastSessionStatus.get(sessionId);
							if (isBusyStatus(status)) addBusySession(sessionId);
							if (isPausedStatus(status)) {
								removeBusySession(sessionId);
								if (
									data.waitingReason !== 'confirmation' &&
									data.waitingReason !== 'scheduled_confirmation' &&
									data.waitingReason !== 'end_incomplete' &&
									notifyCfg?.session_paused?.in_app !== false
								) {
									addNotification(`会话已暂停: ${title}`, 'warning', 3000);
								}
								updateReactExecutionPhase(sessionId, 'idle');
							}
							if (status === 'pending') {
								// Only paused/error → pending is a real resume; Running→Pending
								// (ask answered in-turn) must not toast.
								if (
									(isPausedStatus(previousStatus) ||
										previousStatus === 'error') &&
									notifyCfg?.session_resumed?.in_app !== false
								) {
									addNotification(`会话已恢复: ${title}`, 'info', 3000);
								}
								updateReactExecutionPhase(sessionId, 'queued');
							}
							if (status === 'running' && previousStatus !== 'running') {
								updateReactExecutionPhase(sessionId, 'requesting');
							}
							lastSessionStatus.set(sessionId, status);
							return;
						}
						case 'completed': {
							const title = data.title || data.sessionId;
							const reason = data.reason?.trim();
							if (notifyCfg?.session_completed?.in_app !== false) {
								addNotification(
									reason
										? `会话已完成: ${title}（${reason}）`
										: `会话已完成: ${title}`,
									'success',
								);
							}
							lastSessionStatus.set(data.sessionId, 'completed');
							removeBusySession(data.sessionId);
							updateReactExecutionPhase(data.sessionId, 'idle');
							return;
						}
						case 'error': {
							const message = data.error || data.title || data.sessionId;
							if (notifyCfg?.session_error?.in_app !== false) {
								addNotification(`会话出错: ${message}`, 'error', 5000);
							}
							lastSessionStatus.set(data.sessionId, 'error');
							removeBusySession(data.sessionId);
							updateReactExecutionPhase(data.sessionId, 'idle');
							return;
						}
						case 'deleted':
							if (data.sessionId) {
								lastSessionStatus.delete(data.sessionId);
								removeBusySession(data.sessionId);
								if (get(reactExecutionPhaseStore).sessionId === data.sessionId) {
									updateReactExecutionPhase(data.sessionId, 'idle');
								}
							} else {
								lastSessionStatus.clear();
								clearBusySessions();
								updateReactExecutionPhase(null, 'idle');
							}
							return;
						case 'title_updated':
							return;
					}
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
				}),
				...agentEventListeners({
					'agent:stream_stalled': (event) => {
						// Provider stream went silent while the step is still in flight
						// (first-chunk wait or mid-step gap). Show the factual waiting
						// state — not a guessed "slow" label. Cleared by the next chunk
						// (streaming) or a terminal session event (ready/error).
						const data = event.payload;
						const activeId = appSessionReducer.snapshot().activeSessionId;
						if (data.sessionId && activeId && data.sessionId !== activeId) return;
						if (data.sessionId) {
							updateReactExecutionPhase(data.sessionId, 'waiting_response');
						}
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
						if (data.notificationKind === 'tool_run_completion') {
							toolRunCompletionNotificationGate.notify(data);
							return;
						}
						showAgentNotification(data);
					},
				}),
				...toolRunEventListeners({
					// ToolRun lifecycle is registered globally so tasks stay tracked while
					// the user visits other tabs. Both ToolRun kinds share the named
					// task DTO and use camelCase after this boundary.
					'tool_run:created': (event) => {
						upsertToolRun(event.payload);
						upsertSessionToolRun(event.payload);
					},
					'tool_run:updated': (event) => {
						upsertToolRun(event.payload);
						upsertSessionToolRun(event.payload);
					},
					'tool_run:output': (event) => {
						upsertToolRun(event.payload);
						upsertSessionToolRun(event.payload);
					},
					'tool_run:finished': (event) => {
						const p = event.payload;
						if (p.kind === 'background') {
							upsertToolRun(p);
							upsertSessionToolRun(p);
							finalizeBackgroundToolRunMessages(p);
						} else {
							if (p.status === 'cancelled') {
								appSessionReducer.dispatch({
									type: 'session/scheduled-tool-run-cancelled',
									toolRunId: p.toolRunId,
								});
							}
							upsertSessionToolRun(p);
							// Scheduled ToolRuns leave the pending list at terminal state;
							// execution notifications arrive through `notification:show`.
							removeToolRun(p.toolRunId);
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
			void probeBootstrapStatus();
		} else {
			bootstrapReady = true;
		}

		// Hydrate the ToolRun registry for runs started before this mount
		// (events only cover ToolRuns spawned after the listeners above;
		// fired/cancelled while the UI was away are already gone).
		refreshToolRuns();

		// The execution-phase store subscribe above fires synchronously on mount
		// (phase is 'idle') and triggers the first probe; here we just
		// start the cadence for all subsequent probes (Tauri only).
		if (isTauri()) scheduleLlmProbe();
	});

	onDestroy(() => {
		unsubscribeExecutionPhase();
		removeGlobalErrorHandlers();
		recordingOverlayController.dispose();
		if (llmProbeTimer) clearTimeout(llmProbeTimer);
		if (bootstrapProbeTimer) clearTimeout(bootstrapProbeTimer);
		eventRegistrations?.dispose();
	});

	const tabs: Array<{ id: TabId; label: string; icon: string }> = [
		{ id: 'chat', label: '对话', icon: 'chat' },
		{ id: 'tools', label: '工具', icon: 'briefcase' },
		{ id: 'memory', label: '历史', icon: 'history' },
		{ id: 'settings', label: '设置', icon: 'settings' },
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
			executionPhase={reactExecutionPhase}
			{busySessions}
			{activeSessionStatusLabel}
			{runtime}
			{bootstrapReady}
			{llmConnected}
			llmConnectionDetail={llmConnectionReport
				? llmConnectionReasonText(llmConnectionReport.reason)
				: null}
			{awaitingBackgroundActive}
			{runningBackgroundToolRunCount}
			pendingScheduledToolRunCount={pendingScheduledToolRuns.length}
			onOpenTasks={() => switchTab('memory', 'tasks')}
		/>
	{/snippet}
	{#snippet content()}
		{#each tabs as tab (tab.id)}
			<div
				class="tab-panel"
				class:tab-panel--leaving={leavingTab === tab.id}
				id="workspace-tabpanel-{tab.id}"
				hidden={activeTab !== tab.id && leavingTab !== tab.id}
				inert={activeTab !== tab.id}
				role="tabpanel"
				aria-labelledby="workspace-tab-{tab.id}"
				aria-hidden={activeTab !== tab.id}
				onanimationend={(event) => finishTabLeave(tab.id, event)}
			>
				{#if visited[tab.id]}
					{@const TabComponent =
						tab.id === 'chat' ? undefined : lazyViewComponents[tab.id]}
					{#if tab.id === 'chat'}
						<div
							class="page-shell"
							class:chat-tab-enter={enteringTab === tab.id}
							onanimationend={(event) => finishTabEntry(tab.id, event)}
						>
							{@render children()}
						</div>
					{:else if tab.id === 'tools'}
						<div
							class="page-shell"
							class:page-shell--loading={!lazyViewComponents.tools}
						>
							{#if lazyViewComponents.tools}
								<WorkspaceSurface
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								>
									<TabComponent isVisible={activeTab === 'tools'} />
								</WorkspaceSurface>
							{:else if lazyViewStates.tools === 'error'}
								<WorkspaceSurface
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								>
									<div class="lazy-view-placeholder" role="alert">
										<span>工具页面暂时无法加载</span>
										<MaterialButton
											variant="outlined"
											label="重试"
											onclick={() => retryTabView('tools')}
										/>
									</div>
								</WorkspaceSurface>
							{:else}
								<LoadingState label="正在加载工具…" detail="正在准备工具列表" />
							{/if}
						</div>
					{:else if tab.id === 'memory'}
						<div
							class="page-shell"
							class:page-shell--loading={!lazyViewComponents.memory}
						>
							{#if lazyViewComponents.memory}
								{@const MemoryViewComponent = lazyViewComponents.memory}
								<WorkspaceSurface
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								>
									<MemoryViewComponent
										isVisible={activeTab === 'memory'}
										onNewSession={startNewSessionFromTasks}
										{runningBackgroundToolRuns}
										{pendingScheduledToolRuns}
										{toolRunStatusLabel}
										{sessionTitleFor}
										{toolRunDuration}
										{scheduledToolRunCountdown}
										onOpenSession={openTaskSession}
										onCancel={handleCancelToolRun}
									/>
								</WorkspaceSurface>
							{:else if lazyViewStates.memory === 'error'}
								<WorkspaceSurface
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								>
									<div class="lazy-view-placeholder" role="alert">
										<span>历史页面暂时无法加载</span>
										<MaterialButton
											variant="outlined"
											label="重试"
											onclick={() => retryTabView('memory')}
										/>
									</div>
								</WorkspaceSurface>
							{:else}
								<LoadingState label="正在加载历史…" detail="正在准备历史记录" />
							{/if}
						</div>
					{:else if tab.id === 'settings'}
						<div
							class="page-shell"
							class:page-shell--loading={!lazyViewComponents.settings}
						>
							{#if lazyViewComponents.settings}
								{@const SettingsViewComponent = lazyViewComponents.settings}
								<SettingsViewComponent
									isVisible={activeTab === 'settings'}
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								/>
							{:else if lazyViewStates.settings === 'error'}
								<WorkspaceSurface
									entering={enteringTab === tab.id}
									onAnimationEnd={(/** @type {AnimationEvent} */ event) =>
										finishTabEntry(tab.id, event)}
								>
									<div class="lazy-view-placeholder" role="alert">
										<span>设置页面暂时无法加载</span>
										<MaterialButton
											variant="outlined"
											label="重试"
											onclick={() => retryTabView('settings')}
										/>
									</div>
								</WorkspaceSurface>
							{:else}
								<LoadingState label="正在加载设置…" detail="正在准备设置页面" />
							{/if}
						</div>
					{/if}
				{/if}
			</div>
		{/each}
		<ConfirmationDialog
			open={activeConfirmOpen}
			stepId={activeConfirmRequest?.id || null}
			toolName={activeConfirmRequest?.toolName || ''}
			sessionId={activeConfirmRequest?.sessionId}
			allowSessionScope={Boolean(activeConfirmRequest?.sessionId)}
			sessionTitle={activeConfirmSessionTitle}
			riskLevel={activeConfirmRequest?.riskLevel || 'medium'}
			summary={activeConfirmRequest?.summary || '此操作需要你的许可。'}
			permissionKey={activeConfirmRequest?.permissionKey ||
				activeConfirmRequest?.toolName ||
				''}
			createdAt={activeConfirmRequest?.createdAt || ''}
			deadlineAt={activeConfirmDeadlineAt ?? Number.NaN}
			onDismiss={dismissConfirmation}
			onConfirm={handleConfirm}
		/>
	{/snippet}
</AppShell>
