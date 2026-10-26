import { buildResumeMessages } from './resumeMessages.ts';
import { createSessionRefreshScheduler } from './sessionRefresh.ts';
import { isErrorStatus } from './sessionStatus.ts';
import type { SessionAction, SessionReducer, SessionRunEndStatus } from './sessionReducer.ts';
import type {
	RuntimeSessionListResponse,
	SessionResumeResponse,
} from './contracts/sessionHistory.ts';
import type { SessionResumeTarget } from './sessionIntentStore.ts';

export interface ChatSessionStartupDependencies {
	reducer: SessionReducer;
	dispatch: (action: SessionAction) => void;
	listRuntimeSessions: () => Promise<RuntimeSessionListResponse>;
	getLatestSessionForResume: () => Promise<SessionResumeResponse | null>;
	reopenSession: (request: { sessionId: string }) => Promise<void>;
	refreshToolRuns: () => void;
	getFreshSessionIntent: () => boolean;
	setFreshSessionIntent: (value: boolean) => void;
	hasPersistedFreshSessionIntent: () => boolean;
	clearPersistedFreshSessionIntent: () => void;
	getPendingInteractionRequestIds: (sessionId: string) => string[];
	evictTerminalSessionMemory: (sessionId: string) => void;
	setInitialLoading: (loading: boolean) => void;
	deferResumeTargetClear: () => void;
	warn: (message: string) => void;
	reportError: (
		error: unknown,
		options: { context: string; message: string; log: boolean },
	) => unknown;
}

/**
 * Own chat startup and session resume ordering separately from live session
 * commands. The route supplies store, notification, and view callbacks.
 */
export function createChatSessionStartup(dependencies: ChatSessionStartupDependencies) {
	let loadSessionsSeq = 0;
	let loadSessionsSettled: Promise<void> = Promise.resolve();
	let generation = 0;
	let disposed = false;

	function isCurrentGeneration(expectedGeneration: number) {
		return !disposed && generation === expectedGeneration;
	}

	const reportedRefreshes = new WeakSet<Promise<void>>();
	const actionRefreshes = new WeakSet<Promise<void>>();
	function reportRefreshFailure(error: unknown, request: Promise<void>) {
		if (disposed || actionRefreshes.has(request) || reportedRefreshes.has(request)) return;
		reportedRefreshes.add(request);
		dependencies.reportError(error, {
			context: '+page',
			message: '加载会话列表失败',
			log: false,
		});
	}
	const loadSessionsRefresh = createSessionRefreshScheduler(
		() => loadSessionsNow(),
		300,
		globalThis,
		reportRefreshFailure,
	);

	function retainErroredSession(
		resumeTarget: Pick<SessionResumeTarget, 'sessionId' | 'inputText' | 'title'>,
	) {
		dependencies.dispatch({
			type: 'session/retained-error',
			session: {
				sessionId: resumeTarget.sessionId,
				inputText: resumeTarget.inputText || '',
				title: resumeTarget.title || null,
				status: 'error',
			},
		});
	}

	function hydrateFreshSessionIntent() {
		if (disposed) return;
		if (dependencies.hasPersistedFreshSessionIntent()) {
			dependencies.setFreshSessionIntent(true);
		}
	}

	function applyResumeTarget(resumeTarget: SessionResumeTarget | null) {
		if (!disposed && resumeTarget?.sessionId) {
			// An explicit history choice cancels a pending fresh-start intent.
			dependencies.setFreshSessionIntent(false);
			dependencies.clearPersistedFreshSessionIntent();
			const prevActive = dependencies.reducer.snapshot().activeSessionId;
			dependencies.dispatch({ type: 'session/selected', sessionId: resumeTarget.sessionId });
			if (prevActive && prevActive !== resumeTarget.sessionId) {
				const prevSession = dependencies.reducer
					.snapshot()
					.sessions.find((session) => session.sessionId === prevActive);
				if (
					!prevSession ||
					prevSession.status === 'completed' ||
					prevSession.status === 'error'
				) {
					dependencies.evictTerminalSessionMemory(prevActive);
				}
			}
			const runEndStatus: SessionRunEndStatus | undefined =
				resumeTarget.runEndStatus || (resumeTarget.wasError ? 'error' : undefined);
			if (runEndStatus === 'completed') {
				// Memory history reopens completed sessions before handing off here.
				// Their new persisted state is Paused, so don't restore the old terminal
				// notice or let it override the refreshed runtime summary.
				dependencies.dispatch({
					type: 'session/run-end-notice-cleared',
					sessionId: resumeTarget.sessionId,
				});
			} else if (runEndStatus) {
				dependencies.dispatch({
					type: 'session/run-ended',
					sessionId: resumeTarget.sessionId,
					status: runEndStatus,
					outcome: resumeTarget.runEndOutcome,
					reason:
						resumeTarget.runEndReason ||
						resumeTarget.errorReason ||
						dependencies.reducer.getSessionErrorReason(resumeTarget.sessionId) ||
						(resumeTarget.runEndOutcome === 'turn_completed'
							? '本轮已完成。'
							: runEndStatus === 'error'
								? '本次会话因错误停止，暂未收到更具体的原因。'
								: ''),
				});
				if (runEndStatus === 'error') retainErroredSession(resumeTarget);
			}
			// Keep the target alive through the current mount's initialization.
			dependencies.deferResumeTargetClear();
			if (runEndStatus !== 'error') {
				// Opening history can rehydrate a terminal session into the live
				// paused-session projection. That memory-only transition emits no
				// lifecycle event, so refresh the switcher immediately instead of
				// waiting for the next page mount or manual refresh.
				void loadSessions();
			}
		}
	}

	async function loadSessionsNow(): Promise<void> {
		if (disposed) return;
		const requestGeneration = generation;
		const seq = ++loadSessionsSeq;
		const run = (async () => {
			const result = await dependencies.listRuntimeSessions();
			if (!isCurrentGeneration(requestGeneration) || seq !== loadSessionsSeq) return;
			if (result && result.sessions) {
				const before = dependencies.reducer.snapshot();
				dependencies.dispatch({
					type: 'sessions/loaded',
					sessions: result.sessions.map((session) => ({
						sessionId: session.sessionId,
						status: session.status,
						runEndKind: session.run_end_kind ?? null,
						inputText: session.inputText,
						title: session.title,
						waitingReason: session.waitingReason ?? null,
					})),
					autoSelect: !before.activeSessionId && !dependencies.getFreshSessionIntent(),
				});
				const after = dependencies.reducer.snapshot();
				if (
					after.activeSessionId &&
					!after.sessions.some(
						(session) => session.sessionId === after.activeSessionId,
					) &&
					!after.runEndNotice
				) {
					dependencies.dispatch({ type: 'session/cleared' });
				}
			}
			if (!isCurrentGeneration(requestGeneration)) return;
			// Lifecycle changes can reap background ToolRuns or ScheduleRuns without a
			// matching action-board terminal event.
			dependencies.refreshToolRuns();
		})();
		loadSessionsSettled = run.catch(() => {});
		return run;
	}

	/** Immediate refresh for explicit ToolRun requests; lifecycle bursts share the scheduler. */
	function loadSessions(): Promise<void> {
		if (disposed) return Promise.resolve();
		const run = loadSessionsRefresh.refresh();
		const settled = run.catch((error: unknown) => reportRefreshFailure(error, run));
		loadSessionsSettled = settled;
		return settled;
	}

	/** Action callers own reporting; the shared scheduler still owns query ordering. */
	function refreshSessionListForAction(): Promise<void> {
		if (disposed) return Promise.resolve();
		const run = loadSessionsRefresh.refresh();
		actionRefreshes.add(run);
		loadSessionsSettled = run.catch(() => {});
		return run;
	}

	function scheduleLoadSessions() {
		if (disposed) return;
		loadSessionsRefresh.schedule();
	}

	async function resumeLatestSession(resumeTarget: SessionResumeTarget | null): Promise<void> {
		const requestGeneration = generation;
		if (!isCurrentGeneration(requestGeneration)) return;
		if (
			resumeTarget ||
			dependencies.getFreshSessionIntent() ||
			dependencies.hasPersistedFreshSessionIntent()
		) {
			return;
		}
		await loadSessionsSettled;
		if (!isCurrentGeneration(requestGeneration)) return;
		const current = dependencies.reducer.snapshot();
		if (
			current.activeSessionId &&
			!current.sessions.some((session) => session.sessionId === current.activeSessionId)
		) {
			dependencies.dispatch({ type: 'session/cleared' });
		}
		if (dependencies.reducer.snapshot().activeSessionId) return;

		let latestSession: SessionResumeResponse | null;
		try {
			latestSession = await dependencies.getLatestSessionForResume();
		} catch {
			if (!isCurrentGeneration(requestGeneration)) return;
			dependencies.warn('auto-resume session error');
			return;
		}
		if (!isCurrentGeneration(requestGeneration)) return;
		if (
			!latestSession?.session ||
			dependencies.reducer.snapshot().activeSessionId ||
			dependencies.getFreshSessionIntent()
		) {
			return;
		}
		// A completed session is history and should not be resumed.
		if (latestSession.session.status === 'completed') return;

		const wasError = isErrorStatus(latestSession.session.status);
		const runEndStatus: SessionRunEndStatus | undefined =
			latestSession.session.status === 'paused' ? 'paused' : wasError ? 'error' : undefined;
		const preserveRequestIds = dependencies.getPendingInteractionRequestIds(
			latestSession.session.sessionId,
		);
		dependencies.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: latestSession.session.sessionId,
			messages: buildResumeMessages(latestSession),
			// A recovered confirmation is actionable only after reopen has
			// installed its SessionActor owner. Keep transcript rendering fast,
			// then hydrate interactions once that owner is ready.
			interactions: wasError ? latestSession.interactions : [],
			preserveRequestIds: wasError ? preserveRequestIds : [],
			usage: latestSession.usage,
			llmUsage: latestSession.llm_usage,
			workingDocument: latestSession.workingDocument,
			workingDocumentSequence: latestSession.workingDocumentSequence,
			eventSequenceWatermark: latestSession.eventSequenceWatermark,
		});
		dependencies.dispatch({
			type: 'session/selected',
			sessionId: latestSession.session.sessionId,
		});
		if (runEndStatus) {
			dependencies.dispatch({
				type: 'session/run-ended',
				sessionId: latestSession.session.sessionId,
				status: runEndStatus,
				outcome:
					latestSession.session.run_end_kind === 'turn_completed'
						? 'turn_completed'
						: undefined,
				reason:
					latestSession.session.run_end_reason ||
					dependencies.reducer.getSessionErrorReason(latestSession.session.sessionId) ||
					(latestSession.session.run_end_kind === 'turn_completed'
						? '本轮已完成。'
						: runEndStatus === 'error'
							? '本次会话因错误停止，暂未收到更具体的原因。'
							: ''),
			});
			if (runEndStatus === 'error') {
				retainErroredSession({
					sessionId: latestSession.session.sessionId,
					inputText: latestSession.session.input_text,
					title: latestSession.session.title,
				});
			}
		}
		let actorReady = false;
		try {
			if (!wasError) {
				await dependencies.reopenSession({ sessionId: latestSession.session.sessionId });
				actorReady = true;
			}
		} catch {
			if (!isCurrentGeneration(requestGeneration)) return;
			dependencies.warn('reopen_session error');
		}
		if (!isCurrentGeneration(requestGeneration)) return;
		if (actorReady) {
			dependencies.dispatch({
				type: 'session/interactions-hydrated',
				sessionId: latestSession.session.sessionId,
				requests: latestSession.interactions,
				preserveRequestIds: dependencies.getPendingInteractionRequestIds(
					latestSession.session.sessionId,
				),
			});
		}
		await loadSessions();
	}

	async function loadInitialSessions(resumeTarget: SessionResumeTarget | null): Promise<void> {
		if (disposed) return;
		const requestGeneration = generation;
		const sessionsPromise = loadSessions();
		const resumePromise = resumeLatestSession(resumeTarget);
		try {
			await Promise.all([sessionsPromise, resumePromise]);
		} finally {
			if (isCurrentGeneration(requestGeneration)) dependencies.setInitialLoading(false);
		}
	}

	function dispose() {
		if (disposed) return;
		disposed = true;
		generation += 1;
		loadSessionsSeq += 1;
		loadSessionsRefresh.dispose();
	}

	return {
		dispose,
		hydrateFreshSessionIntent,
		loadInitialSessions,
		loadSessions,
		refreshSessionListForAction,
		applyResumeTarget,
		scheduleLoadSessions,
	};
}
