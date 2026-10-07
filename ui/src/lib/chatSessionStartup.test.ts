import { afterEach, describe, expect, it, vi } from 'vitest';
import { createChatSessionStartup, type ChatSessionStartupDependencies } from './chatSessionStartup.ts';
import type { SessionListResponse, SessionResumeResponse } from './contracts/sessionHistory.ts';
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

function list(...ids: string[]): SessionListResponse {
	return {
		sessions: ids.map((id) => ({
			id,
			input: '',
			summary: '',
			title: null,
			status: 'paused',
			steps: [],
			created_at: '',
			updated_at: '',
			waiting_reason: 'user_input',
		})),
	};
}

function resume(status: SessionResumeResponse['session']['status'] = 'paused'): SessionResumeResponse {
	return {
		session: {
			id: SESSION_ID,
			input_text: '旧会话摘要',
			title: '恢复会话',
			status,
			created_at: '',
			updated_at: '',
		},
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
	};
}

function createHarness(options: {
	listSessions?: () => Promise<SessionListResponse>;
	getLatestSessionForResume?: () => Promise<SessionResumeResponse | null>;
	reopenSession?: (request: { sessionId: string }) => Promise<void>;
} = {}) {
	const reducer = new SessionReducer();
	const actions: SessionAction[] = [];
	const warnings: Array<{ message: string; error: unknown }> = [];
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
		listSessions: async () => {
			sessionListCalls++;
			return options.listSessions ? options.listSessions() : list();
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
		getPendingInteractionIds: (sessionId) =>
			Object.values(reducer.snapshot().interactions)
				.filter((request) => request.sessionId === sessionId && request.status === 'pending')
				.map((request) => request.id),
		evictTerminalSessionMemory: (sessionId) => evicted.push(sessionId),
		setInitialLoading: (loading) => initialLoadingChanges.push(loading),
		deferResumeTargetClear: () => deferredTargetClears.push(0),
		warn: (message, error) => warnings.push({ message, error }),
		reportError: (error, reportOptions) => errors.push({ error, message: reportOptions.message }),
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
	it('waits for session-list selection before auto-restore and preserves restore order', async () => {
		const errorResume = resume('error');
		const sessionsResponse = deferred<SessionListResponse>();
		let sessionsResolved = false;
		const getLatestSessionForResume = vi.fn(async () => {
			expect(sessionsResolved).toBe(true);
			return errorResume;
		});
		const harness = createHarness({
			listSessions: () => sessionsResponse.promise,
			getLatestSessionForResume,
		});
		const pendingIds = ['conf-live'];
		// A reducer-owned pending request must survive a possibly stale resume snapshot.
		harness.reducer.dispatch({
			type: 'session/interaction-upserted',
			request: {
				id: pendingIds[0],
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
			'session/error-shown',
			'session/retained-error',
			'sessions/loaded',
		]);
		expect(harness.actions[1]).toMatchObject({ preserveInteractionIds: ['conf-live'] });
		expect(harness.actions[3]).toMatchObject({
			type: 'session/error-shown',
			reason: '本次会话因错误停止，暂未收到更具体的原因。',
		});
		expect(harness.reopened).toEqual([]);
		expect(harness.initialLoadingChanges).toEqual([false]);
		expect(harness.sessionListCalls).toBe(2);
	});

	it('ignores a session-list response and loading callback after disposal', async () => {
		const sessionsResponse = deferred<SessionListResponse>();
		const getLatestSessionForResume = vi.fn(async () => null);
		const harness = createHarness({
			listSessions: () => sessionsResponse.promise,
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

	it('ignores an auto-restore response and loading callback after disposal', async () => {
		const restoreResponse = deferred<SessionResumeResponse | null>();
		const getLatestSessionForResume = vi.fn(() => restoreResponse.promise);
		const harness = createHarness({
			listSessions: async () => list(),
			getLatestSessionForResume,
		});
		const loading = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(getLatestSessionForResume).toHaveBeenCalledOnce());
		expect(harness.actions.map((action) => action.type)).toEqual(['sessions/loaded']);

		harness.startup.dispose();
		restoreResponse.resolve(resume());
		await loading;

		expect(harness.actions.map((action) => action.type)).toEqual(['sessions/loaded']);
		expect(harness.reopened).toEqual([]);
		expect(harness.initialLoadingChanges).toEqual([]);
	});

	it('hydrates explicit resume targets before startup and evicts a prior terminal session', () => {
		const harness = createHarness();
		harness.reducer.dispatch({
			type: 'sessions/loaded',
			sessions: [{ id: SESSION_ID, status: 'completed' }],
		});
		harness.reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });
		harness.setPersistedIntent(true);

		harness.startup.hydrateFreshSessionIntent();
		harness.startup.processResumeTarget({
			sessionId: OTHER_SESSION_ID,
			wasError: true,
			errorReason: '已停止',
		} satisfies SessionResumeTarget);

		expect(harness.freshIntentChanges).toEqual([true, false]);
		expect(harness.evicted).toEqual([SESSION_ID]);
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.reducer.snapshot().error).toEqual({ sessionId: OTHER_SESSION_ID, reason: '已停止' });
		expect(harness.reducer.snapshot().sessions).toContainEqual(
			expect.objectContaining({ id: OTHER_SESSION_ID, status: 'error' }),
		);
		expect(harness.deferredTargetClears).toEqual([0]);
	});

	it('does not overwrite a session selected while auto-restore is in flight', async () => {
		let finishRestore: ((value: SessionResumeResponse | null) => void) | undefined;
		const restorePromise = new Promise<SessionResumeResponse | null>((resolve) => {
			finishRestore = resolve;
		});
		const getLatestSessionForResume = vi.fn(() => restorePromise);
		const harness = createHarness({ getLatestSessionForResume });
		const initialLoad = harness.startup.loadInitialSessions(null);
		await vi.waitFor(() => expect(getLatestSessionForResume).toHaveBeenCalledOnce());
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		finishRestore?.(resume());
		await initialLoad;

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.actions.some((action) => action.type === 'session/messages/resume-loaded')).toBe(false);
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
