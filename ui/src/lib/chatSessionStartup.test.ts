import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	createChatSessionStartup,
	type ChatSessionStartupDependencies,
} from './chatSessionStartup.ts';
import type {
	RuntimeSessionListResponse,
	SessionResumeResponse,
} from './contracts/sessionHistory.ts';
import type { InteractionRequest } from './contracts/app.ts';
import { SessionReducer, type SessionAction } from './sessionReducer.ts';
import type { SessionResumeTarget } from './sessionIntentStore.ts';

const SESSION_ID = 'ses-00000000000000000000000000000001';
const OTHER_SESSION_ID = 'ses-00000000000000000000000000000002';

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function list(...ids: string[]): RuntimeSessionListResponse {
	return {
		sessions: ids.map((id) => ({
			sessionId: id,
			inputText: '',
			title: null,
			status: 'paused',
			waitingReason: 'user_input',
		})),
	};
}

function resume(
	status: SessionResumeResponse['session']['status'] = 'paused',
): SessionResumeResponse {
	return {
		session: {
			sessionId: SESSION_ID,
			input_text: '旧会话首条输入',
			title: '恢复会话',
			status,
			created_at: '',
			updated_at: '',
			creation: { creator: 'user', method: 'new', source: { kind: 'none' } },
		},
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
		workingDocument: null,
		workingDocumentSequence: null,
		eventSequenceWatermark: 0,
	};
}

function pendingConfirmation(): InteractionRequest {
	return {
		requestId: 'conf-00000000000000000000000000000001',
		sessionId: SESSION_ID,
		owner: { kind: 'session', sessionId: SESSION_ID },
		kind: 'confirm',
		status: 'pending',
		options: [],
		toolName: 'files.write',
		riskLevel: 'high',
		summary: '写入 notes.txt',
		toolIndex: 0,
		permissionKey: 'files.write',
		invocationStepId: 'step-00000000000000000000000000000001',
		createdAt: '2026-10-19T00:00:00Z',
		expiresAt: '2026-10-19T00:05:00Z',
	};
}

function createHarness(
	options: {
		listRuntimeSessions?: () => Promise<RuntimeSessionListResponse>;
		getLatestSessionForResume?: () => Promise<SessionResumeResponse | null>;
		reopenSession?: (request: { sessionId: string }) => Promise<void>;
	} = {},
) {
	const reducer = new SessionReducer();
	const actions: SessionAction[] = [];
	const warnings: string[] = [];
	const errors: Array<{ error: unknown; message: string }> = [];
	const reopened: string[] = [];
	const evicted: string[] = [];
	const freshIntentChanges: boolean[] = [];
	const deferredTargetClears: number[] = [];
	const initialLoadingChanges: boolean[] = [];
	const refreshToolRuns = vi.fn();
	let freshIntent = false;
	let persistedIntent = false;
	let sessionListCalls = 0;

	const dependencies: ChatSessionStartupDependencies = {
		reducer,
		dispatch: (action) => {
			actions.push(action);
			reducer.dispatch(action);
		},
		listRuntimeSessions: async () => {
			sessionListCalls++;
			return options.listRuntimeSessions ? options.listRuntimeSessions() : list();
		},
		getLatestSessionForResume: options.getLatestSessionForResume ?? (async () => resume()),
		reopenSession: async (request) => {
			reopened.push(request.sessionId);
			await options.reopenSession?.(request);
		},
		refreshToolRuns,
		getFreshSessionIntent: () => freshIntent,
		setFreshSessionIntent: (value) => {
			freshIntent = value;
			freshIntentChanges.push(value);
		},
		hasPersistedFreshSessionIntent: () => persistedIntent,
		clearPersistedFreshSessionIntent: () => {
			persistedIntent = false;
		},
		getPendingInteractionRequestIds: (sessionId) =>
			Object.values(reducer.snapshot().interactions)
				.filter(
					(request) => request.sessionId === sessionId && request.status === 'pending',
				)
				.map((request) => request.requestId),
		evictTerminalSessionMemory: (sessionId) => evicted.push(sessionId),
		setInitialLoading: (loading) => initialLoadingChanges.push(loading),
		deferResumeTargetClear: () => deferredTargetClears.push(0),
		warn: (message) => {
			warnings.push(message);
		},
		reportError: (error, reportOptions) =>
			errors.push({ error, message: reportOptions.message }),
	};

	return {
		startup: createChatSessionStartup(dependencies),
		reducer,
		actions,
		warnings,
		errors,
		reopened,
		evicted,
		freshIntentChanges,
		deferredTargetClears,
		initialLoadingChanges,
		refreshToolRuns,
		setPersistedIntent: (value: boolean) => {
			persistedIntent = value;
		},
		get sessionListCalls() {
			return sessionListCalls;
		},
	};
}

afterEach(() => {
	vi.useRealTimers();
});

describe('createChatSessionStartup', () => {
	it('projects runtime session rows into the reducer renderer shape', async () => {
		const runtimeSessions = list(SESSION_ID);
		runtimeSessions.sessions[0]!.inputText = '运行时首条输入';
		const harness = createHarness({ listRuntimeSessions: async () => runtimeSessions });

		await harness.startup.loadSessions();

		expect(harness.reducer.snapshot().sessions).toEqual([
			{
				sessionId: SESSION_ID,
				status: 'paused',
				inputText: '运行时首条输入',
				title: null,
				waitingReason: 'user_input',
			},
		]);
	});

	it('waits for session-list selection before auto-resume and preserves resume order', async () => {
		const errorResume = resume('error');
		const sessionsResponse = deferred<RuntimeSessionListResponse>();
		let sessionsResolved = false;
		const getLatestSessionForResume = vi.fn(async () => {
			expect(sessionsResolved).toBe(true);
			return errorResume;
		});
		const harness = createHarness({
			listRuntimeSessions: () => sessionsResponse.promise,
			getLatestSessionForResume,
		});
		const pendingIds = ['step-fa7d9da560291aec4d4fd57e8812de10'];
		// A reducer-owned pending request must survive a possibly stale resume snapshot.
		harness.reducer.dispatch({
			type: 'session/interaction-upserted',
			request: {
				requestId: pendingIds[0],
				sessionId: SESSION_ID,
				owner: { kind: 'session' as const, sessionId: SESSION_ID },
				kind: 'ask',
				status: 'pending',
				options: [],
				createdAt: '',
			},
		});
		const load = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(harness.sessionListCalls).toBe(1));
		expect(getLatestSessionForResume).not.toHaveBeenCalled();
		sessionsResolved = true;
		sessionsResponse.resolve(list());
		await load;

		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.actions.map((action) => action.type)).toEqual([
			'sessions/loaded',
			'session/messages/resume-loaded',
			'session/selected',
			'session/run-ended',
			'session/retained-error',
			'sessions/loaded',
		]);
		expect(harness.actions[1]).toMatchObject({
			preserveRequestIds: ['step-fa7d9da560291aec4d4fd57e8812de10'],
		});
		expect(harness.actions[3]).toMatchObject({
			type: 'session/run-ended',
			status: 'error',
			reason: '本次会话因错误停止，暂未收到更具体的原因。',
		});
		expect(harness.reopened).toEqual([]);
		expect(harness.initialLoadingChanges).toEqual([false]);
		expect(harness.sessionListCalls).toBe(2);
	});

	it('hydrates replayed confirmations only after auto-resume loads their actor owner', async () => {
		const reopen = deferred<void>();
		const latest = resume();
		const request = pendingConfirmation();
		latest.interactions = [request];
		const harness = createHarness({
			getLatestSessionForResume: async () => latest,
			reopenSession: () => reopen.promise,
		});

		const loading = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(harness.reopened).toEqual([SESSION_ID]));
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.reducer.snapshot().messages[SESSION_ID]).toEqual([]);
		expect(harness.reducer.snapshot().interactions[request.requestId]).toBeUndefined();
		expect(
			harness.actions.some((action) => action.type === 'session/interactions-hydrated'),
		).toBe(false);

		reopen.resolve();
		await loading;

		expect(harness.reducer.snapshot().interactions[request.requestId]).toEqual(request);
		const reopenAction = harness.actions.findIndex(
			(action) => action.type === 'session/interactions-hydrated',
		);
		expect(reopenAction).toBeGreaterThan(
			harness.actions.findIndex((action) => action.type === 'session/messages/resume-loaded'),
		);
		expect(harness.actions[reopenAction]).toMatchObject({
			type: 'session/interactions-hydrated',
			sessionId: SESSION_ID,
			requests: [request],
		});
	});

	it('keeps replayed confirmations hidden when auto-resume cannot load their actor owner', async () => {
		const latest = resume();
		const request = pendingConfirmation();
		latest.interactions = [request];
		const harness = createHarness({
			getLatestSessionForResume: async () => latest,
			reopenSession: async () => {
				throw new Error('actor restore failed');
			},
		});

		await harness.startup.loadInitialSessions(null);

		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.reducer.snapshot().interactions[request.requestId]).toBeUndefined();
		expect(
			harness.actions.some((action) => action.type === 'session/interactions-hydrated'),
		).toBe(false);
		expect(harness.warnings).toContain('reopen_session error');
	});

	it('ignores a session-list response and loading callback after disposal', async () => {
		const sessionsResponse = deferred<RuntimeSessionListResponse>();
		const getLatestSessionForResume = vi.fn(async () => null);
		const harness = createHarness({
			listRuntimeSessions: () => sessionsResponse.promise,
			getLatestSessionForResume,
		});
		const loading = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(harness.sessionListCalls).toBe(1));

		harness.startup.dispose();
		sessionsResponse.resolve(list(SESSION_ID));
		await loading;

		expect(harness.actions).toEqual([]);
		expect(harness.refreshToolRuns).not.toHaveBeenCalled();
		expect(getLatestSessionForResume).not.toHaveBeenCalled();
		expect(harness.initialLoadingChanges).toEqual([]);
	});

	it('ignores an auto-resume response and loading callback after disposal', async () => {
		const resumeResponse = deferred<SessionResumeResponse | null>();
		const getLatestSessionForResume = vi.fn(() => resumeResponse.promise);
		const harness = createHarness({
			listRuntimeSessions: async () => list(),
			getLatestSessionForResume,
		});
		const loading = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(getLatestSessionForResume).toHaveBeenCalledOnce());
		expect(harness.actions.map((action) => action.type)).toEqual(['sessions/loaded']);

		harness.startup.dispose();
		resumeResponse.resolve(resume());
		await loading;

		expect(harness.actions.map((action) => action.type)).toEqual(['sessions/loaded']);
		expect(harness.reopened).toEqual([]);
		expect(harness.initialLoadingChanges).toEqual([]);
	});

	it('hydrates explicit resume targets before startup and evicts a prior terminal session', () => {
		const harness = createHarness();
		harness.reducer.dispatch({
			type: 'sessions/loaded',
			sessions: [{ sessionId: SESSION_ID, status: 'completed' }],
		});
		harness.reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });
		harness.setPersistedIntent(true);

		harness.startup.hydrateFreshSessionIntent();
		harness.startup.applyResumeTarget({
			sessionId: OTHER_SESSION_ID,
			wasError: true,
			inputText: '首条输入',
			errorReason: '已停止',
		} satisfies SessionResumeTarget);

		expect(harness.freshIntentChanges).toEqual([true, false]);
		expect(harness.evicted).toEqual([SESSION_ID]);
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.reducer.snapshot().sessions).toContainEqual(
			expect.objectContaining({ sessionId: OTHER_SESSION_ID, inputText: '首条输入' }),
		);
		expect(harness.reducer.snapshot().runEndNotice).toEqual({
			sessionId: OTHER_SESSION_ID,
			status: 'error',
			reason: '已停止',
		});
		expect(harness.reducer.snapshot().sessions).toContainEqual(
			expect.objectContaining({ sessionId: OTHER_SESSION_ID, status: 'error' }),
		);
		expect(harness.deferredTargetClears).toEqual([0]);
	});

	it('clears a completed notice when history has reopened the session as paused', async () => {
		const harness = createHarness({ listRuntimeSessions: async () => list(SESSION_ID) });
		harness.reducer.dispatch({
			type: 'sessions/loaded',
			sessions: [{ sessionId: SESSION_ID, status: 'completed' }],
		});
		harness.reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });
		harness.reducer.dispatch({
			type: 'session/run-ended',
			sessionId: SESSION_ID,
			status: 'completed',
			reason: '用户主动结束会话',
		});

		harness.startup.applyResumeTarget({
			sessionId: SESSION_ID,
			runEndStatus: 'completed',
			wasError: false,
		});

		await vi.waitFor(() =>
			expect(harness.reducer.snapshot().sessions).toContainEqual(
				expect.objectContaining({ sessionId: SESSION_ID, status: 'paused' }),
			),
		);
		expect(harness.reducer.snapshot().runEndNotice).toBeNull();
		expect(harness.actions).toContainEqual({
			type: 'session/run-end-notice-cleared',
			sessionId: SESSION_ID,
		});
	});

	it('does not overwrite a session selected while auto-resume is in flight', async () => {
		let finishResume: ((value: SessionResumeResponse | null) => void) | undefined;
		const resumePromise = new Promise<SessionResumeResponse | null>((resolve) => {
			finishResume = resolve;
		});
		const getLatestSessionForResume = vi.fn(() => resumePromise);
		const harness = createHarness({ getLatestSessionForResume });
		const initialLoad = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(getLatestSessionForResume).toHaveBeenCalledOnce());
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		finishResume?.(resume());
		await initialLoad;

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(
			harness.actions.some((action) => action.type === 'session/messages/resume-loaded'),
		).toBe(false);
		expect(harness.reopened).toEqual([]);
	});

	it('uses the shared refresh scheduler for lifecycle bursts and disposes pending work', async () => {
		vi.useFakeTimers();
		const harness = createHarness();
		harness.startup.scheduleLoadSessions();
		harness.startup.scheduleLoadSessions();
		await vi.advanceTimersByTimeAsync(300);
		expect(harness.sessionListCalls).toBe(1);
		expect(harness.refreshToolRuns).toHaveBeenCalledOnce();

		harness.startup.scheduleLoadSessions();
		harness.startup.dispose();
		await vi.advanceTimersByTimeAsync(300);
		expect(harness.sessionListCalls).toBe(1);
	});
});
