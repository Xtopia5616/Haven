import { describe, expect, it } from 'vitest';
import {
	createChatSessionController,
	type ChatSessionControllerDependencies,
} from './chatSessionController.ts';
import {
	forkAndRefreshSessionList,
	submitAndRefreshSessionList,
} from './chatSessionOperationResults.ts';
import type { InteractionRequest } from './contracts/app.ts';
import { SessionReducer, type SessionAction } from './sessionReducer.ts';
import type { TranscriptSubmissionResultView } from './contracts/transcriptSubmissionResult.ts';
import type { CanonicalRole } from './contracts/generatedCommands.ts';
import type { SessionResumeResponse } from './contracts/sessionHistory.ts';
import type { SessionMessagePresentationType } from './streaming.ts';
import type { ResumeTranscriptProjectionInput } from './resumeMessages.ts';
import { createChatSessionEventHandler } from './chatSessionEventHandlers.ts';
import { createChatSessionStartup } from './chatSessionStartup.ts';

const SESSION_ID = 'ses-00000000000000000000000000000001';
const OTHER_SESSION_ID = 'ses-00000000000000000000000000000002';
const CREATED_SESSION_ID = 'ses-00000000000000000000000000000003';
const USER_MESSAGE_ID = 'msg-00000000000000000000000000000001';

function resumeResponseFixture(
	overrides: Partial<ResumeTranscriptProjectionInput> & { eventSequenceWatermark?: number } = {},
) {
	return {
		session: { id: SESSION_ID, status: 'paused' },
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
		eventSequenceWatermark: 0,
		...overrides,
	};
}

function makeHarness(
	options: {
		runCommand?: (command: string, args?: unknown) => unknown | Promise<unknown>;
		submit?: (
			text: string,
			args: Parameters<ChatSessionControllerDependencies['submitTranscript']>[1],
		) => TranscriptSubmissionResultView | Promise<TranscriptSubmissionResultView>;
	} = {},
) {
	const reducer = new SessionReducer();
	reducer.dispatch({
		type: 'sessions/loaded',
		sessions: [
			{ sessionId: SESSION_ID, status: 'error', title: '旧会话' },
			{ sessionId: OTHER_SESSION_ID, status: 'paused', title: '目标会话' },
		],
	});
	reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });

	const commandCalls: Array<{ command: string; args?: unknown }> = [];
	const submitCalls: Array<{
		text: string;
		args: Parameters<ChatSessionControllerDependencies['submitTranscript']>[1];
	}> = [];
	const actions: SessionAction[] = [];
	const notifications: Array<{ message: string; type: string; duration?: number }> = [];
	const reportedErrors: Array<{ error: unknown; message?: string }> = [];
	const stepBlockClears: string[] = [];
	const continuePending: boolean[] = [];
	const interruptPending: boolean[] = [];
	const rollbackLoading: boolean[] = [];
	const autoFollow: boolean[] = [];
	let freshSessionIntent = false;
	let persistedIntentClears = 0;
	let loadSessionsCalls = 0;

	const runCommand = async (command: string, args?: unknown): Promise<unknown> => {
		commandCalls.push({ command, args });
		return options.runCommand
			? await options.runCommand(command, args)
			: command === 'get_session_for_resume'
				? resumeResponseFixture()
				: undefined;
	};
	const commands: ChatSessionControllerDependencies['commands'] = {
		rollbackSession: async (request) => {
			await runCommand('rollback_session', request);
		},
		endSession: async (request) => {
			await runCommand('end_session', request);
		},
		interruptSession: async (request) => {
			await runCommand('interrupt_session', request);
		},
		continueSession: async (request) => {
			await runCommand('continue_session', request);
		},
		forkSessionFromMessage: async (request) =>
			(await runCommand('fork_session_from_message', request)) as string,
		getSessionForResume: async (request) =>
			(await runCommand('get_session_for_resume', request)) as SessionResumeResponse,
		getSessionEventSequenceWatermark: async (request) =>
			Number((await runCommand('get_session_event_sequence_watermark', request)) ?? 0),
	};

	const dependencies: ChatSessionControllerDependencies = {
		commands,
		submitTranscript: async (text, args) => {
			submitCalls.push({ text, args });
			return options.submit
				? await options.submit(text, args)
				: { kind: 'supplemented', sessionId: SESSION_ID };
		},
		reducer,
		dispatch: (action) => {
			actions.push(action);
			reducer.dispatch(action);
		},
		getActiveSessionId: () => reducer.snapshot().activeSessionId,
		getSessionSnapshot: () => reducer.snapshot().sessions,
		notify: (message, type, duration) => notifications.push({ message, type, duration }),
		reportError: (error, reportOptions) => {
			reportedErrors.push({ error, message: reportOptions.message });
		},
		loadSessions: async () => {
			loadSessionsCalls++;
		},
		clearStepBlockIds: (sessionId) => stepBlockClears.push(sessionId),
		setFreshSessionIntent: (value) => {
			freshSessionIntent = value;
		},
		clearPersistedFreshSessionIntent: () => persistedIntentClears++,
		setRollbackLoading: (loading) => rollbackLoading.push(loading),
		setForkLoading: () => {},
		setContinuePending: (pending) => continuePending.push(pending),
		setInterruptPending: (pending) => interruptPending.push(pending),
		setAutoFollow: (follow) => autoFollow.push(follow),
	};

	return {
		controller: createChatSessionController(dependencies),
		setFreshSessionIntent: dependencies.setFreshSessionIntent,
		reducer,
		commandCalls,
		submitCalls,
		actions,
		notifications,
		reportedErrors,
		stepBlockClears,
		continuePending,
		interruptPending,
		rollbackLoading,
		autoFollow,
		get freshSessionIntent() {
			return freshSessionIntent;
		},
		get persistedIntentClears() {
			return persistedIntentClears;
		},
		get loadSessionsCalls() {
			return loadSessionsCalls;
		},
	};
}

function addMessages(
	reducer: SessionReducer,
	...messages: Array<{
		id: string;
		role: CanonicalRole;
		content: string;
		type?: SessionMessagePresentationType;
	}>
) {
	reducer.dispatch({
		type: 'session/messages/resume-loaded',
		sessionId: SESSION_ID,
		messages,
	});
}

function pendingInteraction(sessionId = SESSION_ID): InteractionRequest {
	return {
		requestId: 'step-aac817798dc536ab69caa990732988f1',
		sessionId,
		owner: { kind: 'session' as const, sessionId },
		kind: 'ask',
		status: 'pending',
		options: ['是', '否'],
		createdAt: '2026-09-25T00:00:00Z',
	};
}

describe('ChatSessionController fork', () => {
	function forkHarness() {
		return makeHarness({
			runCommand: (command) =>
				command === 'fork_session_from_message'
					? CREATED_SESSION_ID
					: resumeResponseFixture(),
		});
	}

	it('keeps a created child successful when list refresh fails and still selects it', async () => {
		const harness = forkHarness();
		const error = new Error('refresh failed');
		const result = await forkAndRefreshSessionList(
			() => harness.controller.forkSessionFromMessage(USER_MESSAGE_ID),
			async () => {
				throw error;
			},
			(fork) => harness.controller.selectForkedSession(fork),
		);
		expect(result).toMatchObject({
			kind: 'created',
			sessionId: CREATED_SESSION_ID,
			refresh: { kind: 'failed', error },
			selection: { kind: 'switched' },
		});
		expect(harness.reducer.snapshot().activeSessionId).toBe(CREATED_SESSION_ID);
		expect(harness.notifications).toEqual([]);
		expect(harness.reportedErrors).toEqual([]);
	});

	it('does not select a child after a newer choice during list refresh', async () => {
		const harness = forkHarness();
		const result = await forkAndRefreshSessionList(
			() => harness.controller.forkSessionFromMessage(USER_MESSAGE_ID),
			async () => {
				harness.controller.beginUserSessionSelection();
			},
			(fork) => harness.controller.selectForkedSession(fork),
		);
		expect(result).toMatchObject({ kind: 'created', selection: { kind: 'superseded' } });
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.commandCalls.map((call) => call.command)).toEqual([
			'fork_session_from_message',
		]);
	});

	it('returns created when selecting the persisted child fails', async () => {
		const error = new Error('resume failed');
		const harness = makeHarness({
			runCommand: (command) => {
				if (command === 'fork_session_from_message') return CREATED_SESSION_ID;
				throw error;
			},
		});
		const result = await forkAndRefreshSessionList(
			() => harness.controller.forkSessionFromMessage(USER_MESSAGE_ID),
			async () => {},
			(fork) => harness.controller.selectForkedSession(fork),
		);
		expect(result).toMatchObject({ kind: 'created', selection: { kind: 'failed', error } });
	});

	it('does not select a late child after switching away and back while creating', async () => {
		let finish!: (value: string) => void;
		const harness = makeHarness({
			runCommand: () =>
				new Promise<string>((resolve) => {
					finish = resolve;
				}),
		});
		const pending = harness.controller.forkSessionFromMessage(USER_MESSAGE_ID);
		harness.controller.beginUserSessionSelection();
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		harness.controller.beginUserSessionSelection();
		harness.reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });
		finish(CREATED_SESSION_ID);
		const fork = await pending;
		expect(fork.kind).toBe('created');
		if (fork.kind !== 'created') throw new Error('expected created fork');
		expect(await harness.controller.selectForkedSession(fork)).toEqual({ kind: 'superseded' });
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
	});

	it.each([false, true])(
		'fences child resume after a newer selection (failure=%s)',
		async (fail) => {
			let finish!: (value: unknown) => void;
			let reject!: (error: Error) => void;
			const harness = makeHarness({
				runCommand: (command) =>
					command === 'fork_session_from_message'
						? CREATED_SESSION_ID
						: new Promise((resolve, rejectPromise) => {
								finish = resolve;
								reject = rejectPromise;
							}),
			});
			const fork = await harness.controller.forkSessionFromMessage(USER_MESSAGE_ID);
			if (fork.kind !== 'created') throw new Error('expected created fork');
			const pending = harness.controller.selectForkedSession(fork);
			harness.controller.beginUserSessionSelection();
			harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
			if (fail) reject(new Error('late resume failure'));
			else finish(resumeResponseFixture());
			expect(await pending).toEqual({ kind: 'superseded' });
			expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
			expect(
				harness.actions.filter(
					(action) => action.type === 'session/messages/resume-loaded',
				),
			).toEqual([]);
		},
	);

	it('rejects unsaved messages and avoids refresh after creation fails', async () => {
		const error = new Error('create failed');
		const harness = makeHarness({
			runCommand: () => {
				throw error;
			},
		});
		expect(await harness.controller.forkSessionFromMessage('pending')).toEqual({
			kind: 'rejected',
			reason: 'message-not-persisted',
		});
		let refreshes = 0;
		const result = await forkAndRefreshSessionList(
			() => harness.controller.forkSessionFromMessage(USER_MESSAGE_ID),
			async () => {
				refreshes++;
			},
			(fork) => harness.controller.selectForkedSession(fork),
		);
		expect(result).toEqual({ kind: 'failed', error });
		expect(refreshes).toBe(0);
	});

	it('rejects concurrent creates without duplicating the backend operation', async () => {
		let finish!: (value: string) => void;
		const harness = makeHarness({
			runCommand: () =>
				new Promise<string>((resolve) => {
					finish = resolve;
				}),
		});
		const pending = harness.controller.forkSessionFromMessage(USER_MESSAGE_ID);
		expect(await harness.controller.forkSessionFromMessage(USER_MESSAGE_ID)).toEqual({
			kind: 'ignored',
			reason: 'already-in-flight',
		});
		finish(CREATED_SESSION_ID);
		expect(await pending).toMatchObject({ kind: 'created', sessionId: CREATED_SESSION_ID });
		expect(harness.commandCalls).toHaveLength(1);
	});
});

describe('ChatSessionController rollback', () => {
	it('pauses and restores the draft when rolling back a persisted user message', async () => {
		const harness = makeHarness();
		const result = await harness.controller.confirmRollbackAction({
			stepNumber: 4,
			role: 'user' as const,
			content: '重新编辑这段',
			messageId: USER_MESSAGE_ID,
		});

		expect(harness.commandCalls[0]).toEqual({
			command: 'rollback_session',
			args: {
				sessionId: SESSION_ID,
				targetStep: 4,
				pause: true,
				targetMessageId: USER_MESSAGE_ID,
			},
		});
		expect(result).toEqual({
			kind: 'rolled-back',
			sessionId: SESSION_ID,
			stepNumber: 4,
			role: 'user',
			content: '重新编辑这段',
			selectionCurrent: true,
			transcriptRefresh: 'loaded',
		});
		expect(harness.actions.map((action) => action.type)).toContain('session/replay-reset');
		expect(harness.stepBlockClears).toEqual([SESSION_ID]);
		expect(harness.rollbackLoading).toEqual([true, false]);
		expect(harness.notifications).toEqual([]);
		expect(harness.loadSessionsCalls).toBe(0);
	});

	it('returns the committed action rollback identity for route feedback', async () => {
		const harness = makeHarness();
		const result = await harness.controller.confirmRollbackAction({
			stepNumber: 7,
			role: 'assistant' as const,
			content: '',
			messageId: 'step-00000000000000000000000000000007',
		});
		expect(result).toMatchObject({ kind: 'rolled-back', selectionCurrent: true });

		expect(harness.commandCalls[0]?.args).toEqual({
			sessionId: SESSION_ID,
			targetStep: 7,
			pause: false,
			targetMessageId: null,
		});
		expect(harness.notifications).toEqual([]);
	});

	it('does not send a timeline step identity as a rollback message target', async () => {
		const harness = makeHarness();
		const result = await harness.controller.confirmRollbackAction({
			stepNumber: 8,
			role: 'user' as const,
			content: '正在保存',
			messageId: 'step-00000000000000000000000000000008',
		});
		expect(result).toEqual({ kind: 'rejected', reason: 'message-not-persisted' });

		expect(harness.commandCalls).toEqual([]);
		expect(harness.notifications).toEqual([]);
	});
});

describe('ChatSessionController continue', () => {
	it('sends 继续 after generated assistant output and keeps the authoritative reload ordered', async () => {
		const harness = makeHarness();
		addMessages(
			harness.reducer,
			{ id: USER_MESSAGE_ID, role: 'user' as const, content: '写一首诗' },
			{
				id: 'step-00000000000000000000000000000014',
				role: 'assistant' as const,
				content: '已有一段',
				type: 'thought',
			},
		);

		await harness.controller.handleContinue();

		expect(harness.commandCalls.map((call) => call.command)).toEqual([
			'continue_session',
			'get_session_for_resume',
		]);
		expect(harness.commandCalls[0]?.args).toEqual({ sessionId: SESSION_ID });
		expect(harness.submitCalls.map((call) => call.text)).toEqual(['继续']);
		expect(harness.actions.map((action) => action.type)).toContain(
			'session/run-end-notice-cleared',
		);
		expect(harness.actions.map((action) => action.type)).toContain('session/replay-reset');
		expect(harness.autoFollow).toEqual([true]);
		expect(harness.continuePending).toEqual([true, false]);
		// Continue owns one session-list refresh after its follow-up has settled.
		expect(harness.loadSessionsCalls).toBe(1);
	});

	it('resubmits the original user turn when it did not survive the resume reload', async () => {
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_for_resume' ? resumeResponseFixture() : undefined,
		});
		addMessages(harness.reducer, {
			id: USER_MESSAGE_ID,
			role: 'user' as const,
			content: '打开计算器',
		});

		await harness.controller.handleContinue();

		expect(harness.submitCalls.map((call) => call.text)).toEqual(['打开计算器']);
	});

	it('does not resubmit an original user turn still present in the authoritative snapshot', async () => {
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_for_resume'
					? resumeResponseFixture({
							messages: [
								{
									messageId: USER_MESSAGE_ID,
									role: 'user' as const,
									content: '打开计算器',
									created_at: '2026-09-25T00:00:00Z',
								},
							],
						})
					: undefined,
		});
		addMessages(harness.reducer, {
			id: USER_MESSAGE_ID,
			role: 'user' as const,
			content: '打开计算器',
		});

		await harness.controller.handleContinue();

		expect(harness.submitCalls).toEqual([]);
	});

	it('does not continue an older session into a newer selection', async () => {
		let resolveContinue!: () => void;
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'continue_session'
					? new Promise<void>((resolve) => {
							resolveContinue = resolve;
						})
					: resumeResponseFixture(),
		});
		addMessages(harness.reducer, {
			id: USER_MESSAGE_ID,
			role: 'user' as const,
			content: '原会话输入',
		});

		const pendingContinue = harness.controller.handleContinue();
		harness.controller.beginUserSessionSelection();
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		resolveContinue();
		await pendingContinue;

		expect(harness.submitCalls).toEqual([]);
		expect(harness.commandCalls.map((call) => call.command)).toEqual(['continue_session']);
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
	});

	it.each(
		(['resolved', 'rejected'] as const).flatMap((outcome) =>
			(['other-session', 'new-session', 'away-and-back'] as const).flatMap((selection) =>
				(['original-user', 'assistant-output'] as const).map((strategy) => ({
					outcome,
					selection,
					strategy,
				})),
			),
		),
	)(
		'abandons $strategy continuation after $selection during a $outcome reload',
		async ({ outcome, selection, strategy }) => {
			let resolveReload!: (value: ReturnType<typeof resumeResponseFixture>) => void;
			let rejectReload!: (error: Error) => void;
			let signalReloadStarted!: () => void;
			const reloadStarted = new Promise<void>((resolve) => {
				signalReloadStarted = resolve;
			});
			const reload = new Promise<ReturnType<typeof resumeResponseFixture>>(
				(resolve, reject) => {
					resolveReload = resolve;
					rejectReload = reject;
				},
			);
			const harness = makeHarness({
				runCommand: (command) => {
					if (command !== 'get_session_for_resume') return;
					signalReloadStarted();
					return reload;
				},
			});
			addMessages(harness.reducer, {
				id: USER_MESSAGE_ID,
				role: 'user',
				content: '原会话输入',
			});
			if (strategy === 'assistant-output') {
				addMessages(
					harness.reducer,
					{ id: USER_MESSAGE_ID, role: 'user', content: '原会话输入' },
					{
						id: 'step-00000000000000000000000000000014',
						role: 'assistant',
						content: '已有一段',
						type: 'thought',
					},
				);
			}

			const pendingContinue = harness.controller.handleContinue();
			await reloadStarted;
			harness.controller.beginUserSessionSelection();
			const selectedSessionId = selection === 'new-session' ? null : OTHER_SESSION_ID;
			harness.reducer.dispatch({ type: 'session/selected', sessionId: selectedSessionId });
			if (selection === 'away-and-back') {
				harness.controller.beginUserSessionSelection();
				harness.reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });
			}
			if (outcome === 'resolved') resolveReload(resumeResponseFixture());
			else rejectReload(new Error('reload unavailable'));
			await pendingContinue;

			expect(harness.submitCalls).toEqual([]);
			expect(harness.autoFollow).toEqual([]);
			expect(
				harness.actions.filter(
					(action) =>
						action.type === 'session/replay-reset' ||
						action.type === 'session/messages/resume-loaded',
				),
			).toEqual([]);
			expect(harness.reducer.snapshot().activeSessionId).toBe(
				selection === 'away-and-back' ? SESSION_ID : selectedSessionId,
			);
			expect(
				harness.reducer.getMessages(SESSION_ID).map((message) => message.content),
			).toContain('原会话输入');
			expect(harness.continuePending).toEqual([true, false]);
			expect(harness.reportedErrors).toEqual([]);
		},
	);

	it('preserves pending interactions when a resume snapshot contains none', async () => {
		const harness = makeHarness();
		harness.reducer.dispatch({
			type: 'session/interaction-upserted',
			request: pendingInteraction(),
		});

		await harness.controller.resyncSessionMessages(SESSION_ID);

		expect(
			harness.reducer.snapshot().interactions['step-aac817798dc536ab69caa990732988f1']
				?.status,
		).toBe('pending');
		const resumeAction = harness.actions.find(
			(action) => action.type === 'session/messages/resume-loaded',
		);
		expect(resumeAction).toMatchObject({
			preserveRequestIds: ['step-aac817798dc536ab69caa990732988f1'],
		});
	});

	it('ignores a duplicate continue request while the first command is in flight', async () => {
		let finishContinue: (() => void) | undefined;
		const continueCommand = new Promise<void>((resolve) => {
			finishContinue = resolve;
		});
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'continue_session' ? continueCommand : resumeResponseFixture(),
		});

		const first = harness.controller.handleContinue();
		await harness.controller.handleContinue();
		expect(
			harness.commandCalls.filter((call) => call.command === 'continue_session'),
		).toHaveLength(1);
		finishContinue?.();
		await first;
	});

	it('ignores a duplicate rollback while the first command is in flight', async () => {
		let finishRollback: (() => void) | undefined;
		const rollbackCommand = new Promise<void>((resolve) => {
			finishRollback = resolve;
		});
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'rollback_session' ? rollbackCommand : resumeResponseFixture(),
		});
		const request = {
			stepNumber: 7,
			role: 'assistant' as const,
			content: '',
			messageId: 'step-00000000000000000000000000000007',
		};

		const first = harness.controller.confirmRollbackAction(request);
		const duplicate = await harness.controller.confirmRollbackAction(request);
		expect(duplicate).toEqual({ kind: 'ignored', reason: 'already-in-flight' });

		expect(
			harness.commandCalls.filter((call) => call.command === 'rollback_session'),
		).toHaveLength(1);
		expect(harness.commandCalls[0]?.args).toEqual({
			sessionId: SESSION_ID,
			targetStep: 7,
			pause: false,
			targetMessageId: null,
		});
		finishRollback?.();
		await first;
	});

	it('does not restore an old rollback draft after the user changes sessions', async () => {
		let resolveRollback!: () => void;
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'rollback_session'
					? new Promise<void>((resolve) => {
							resolveRollback = resolve;
						})
					: resumeResponseFixture(),
		});
		const pendingRollback = harness.controller.confirmRollbackAction({
			stepNumber: 4,
			role: 'user' as const,
			content: '旧会话草稿',
			messageId: USER_MESSAGE_ID,
		});

		harness.controller.beginUserSessionSelection();
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		resolveRollback();
		const result = await pendingRollback;

		expect(result).toMatchObject({ kind: 'rolled-back', selectionCurrent: false });
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
	});

	it('does not let a pre-rollback resume snapshot overwrite the rolled-back transcript', async () => {
		let resolveStaleSnapshot!: (value: SessionResumeResponse) => void;
		let resumeCall = 0;
		const staleSnapshot = new Promise<SessionResumeResponse>((resolve) => {
			resolveStaleSnapshot = resolve;
		});
		const harness = makeHarness({
			runCommand: (command) => {
				if (command !== 'get_session_for_resume') return undefined;
				resumeCall++;
				if (resumeCall === 1) return staleSnapshot;
				return resumeResponseFixture({
					eventSequenceWatermark: 12,
					messages: [
						{
							messageId: USER_MESSAGE_ID,
							role: 'assistant',
							content: '回滚后的内容',
							created_at: '2026-10-20T00:00:00Z',
						},
					],
				});
			},
		});

		const pendingStaleSync = harness.controller.resyncSessionMessages(SESSION_ID);
		await harness.controller.confirmRollbackAction({
			stepNumber: 7,
			role: 'assistant',
			content: '',
			messageId: 'step-00000000000000000000000000000007',
		});
		resolveStaleSnapshot(
			resumeResponseFixture({
				eventSequenceWatermark: 8,
				messages: [
					{
						messageId: USER_MESSAGE_ID,
						role: 'assistant',
						content: '回滚前的内容',
						created_at: '2026-10-19T00:00:00Z',
					},
				],
			}) as unknown as SessionResumeResponse,
		);
		await pendingStaleSync;

		expect(harness.reducer.getMessages(SESSION_ID)).toMatchObject([
			{ id: USER_MESSAGE_ID, content: '回滚后的内容' },
		]);
	});
});

describe('ChatSessionController event reconciliation', () => {
	it('backfills a silently missed committed event only after a stable cursor advance', async () => {
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_event_sequence_watermark'
					? 8
					: command === 'get_session_for_resume'
						? resumeResponseFixture({ eventSequenceWatermark: 8 })
						: undefined,
		});

		await harness.controller.reconcileSessionEvents(SESSION_ID);

		expect(harness.commandCalls.map((call) => call.command)).toEqual([
			'get_session_event_sequence_watermark',
			'get_session_for_resume',
			'get_session_event_sequence_watermark',
		]);
		expect(harness.reducer.getEventSequenceWatermark(SESSION_ID)).toBe(8);
	});

	it('discards a snapshot while the durable cursor moves and recovers on the next poll', async () => {
		const watermarks = [10, 11, 11, 11];
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_event_sequence_watermark'
					? watermarks.shift()
					: command === 'get_session_for_resume'
						? resumeResponseFixture({
								eventSequenceWatermark: 11,
								messages: [
									{
										messageId: USER_MESSAGE_ID,
										role: 'assistant',
										content: 'durable transcript',
										created_at: '2026-10-24T00:00:00Z',
									},
								],
							})
						: undefined,
		});

		await harness.controller.reconcileSessionEvents(SESSION_ID);
		expect(harness.reducer.getEventSequenceWatermark(SESSION_ID)).toBe(0);
		expect(harness.reducer.getMessages(SESSION_ID)).toEqual([]);

		await harness.controller.reconcileSessionEvents(SESSION_ID);
		expect(harness.reducer.getEventSequenceWatermark(SESSION_ID)).toBe(11);
		expect(harness.reducer.getMessages(SESSION_ID)).toMatchObject([
			{ id: USER_MESSAGE_ID, content: 'durable transcript' },
		]);
	});

	it('checks the durable cursor without loading a transcript when no event was missed', async () => {
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_event_sequence_watermark' ? 0 : undefined,
		});

		await harness.controller.reconcileSessionEvents(SESSION_ID);

		expect(harness.commandCalls.map((call) => call.command)).toEqual([
			'get_session_event_sequence_watermark',
		]);
	});
});

describe('ChatSessionController session guards', () => {
	it('selects a newly created session after submitTranscript resolves', async () => {
		const harness = makeHarness({
			submit: () => ({ kind: 'session_created', sessionId: OTHER_SESSION_ID }),
		});

		const result = await harness.controller.submitMessage('新会话第一条消息');

		expect(result).toEqual({ kind: 'submitted', sessionId: OTHER_SESSION_ID });
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.loadSessionsCalls).toBe(0);
	});

	it('does not let an older pending submit override a newer session selection', async () => {
		let resolveSubmit!: (result: TranscriptSubmissionResultView) => void;
		const harness = makeHarness({
			submit: () =>
				new Promise((resolve) => {
					resolveSubmit = resolve;
				}),
		});

		const pendingSubmit = harness.controller.submitMessage('创建会话');
		harness.controller.beginUserSessionSelection();
		harness.reducer.dispatch({ type: 'session/selected', sessionId: OTHER_SESSION_ID });
		resolveSubmit({ kind: 'session_created', sessionId: CREATED_SESSION_ID });
		expect(await pendingSubmit).toEqual({ kind: 'submitted', sessionId: CREATED_SESSION_ID });

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
	});

	it('keeps a committed submission successful when the session-list refresh fails', async () => {
		const refreshError = new Error('session list unavailable');
		const harness = makeHarness({
			submit: () => ({ kind: 'session_created', sessionId: CREATED_SESSION_ID }),
		});

		const result = await submitAndRefreshSessionList(
			() => harness.controller.submitMessage('创建并发送'),
			async () => {
				throw refreshError;
			},
		);

		expect(result).toEqual({
			kind: 'submitted',
			sessionId: CREATED_SESSION_ID,
			refresh: { kind: 'failed', error: refreshError },
		});
		expect(harness.reducer.snapshot().activeSessionId).toBe(CREATED_SESSION_ID);
	});

	it('preserves a newer draft after end fails and refuses an unrelated created event', async () => {
		let rejectEnd!: (error: Error) => void;
		const harness = makeHarness({
			runCommand: () =>
				new Promise((_, reject) => {
					rejectEnd = reject;
				}),
		});
		const ending = harness.controller.endSession();
		// Match the route's explicit New Session intent before the old command settles.
		harness.controller.beginUserSessionSelection();
		harness.setFreshSessionIntent(true);
		harness.reducer.dispatch({ type: 'session/cleared' });
		rejectEnd(new Error('late end failure'));
		await ending;

		const onLifecycle = createChatSessionEventHandler({
			getActiveSessionId: () => harness.reducer.snapshot().activeSessionId,
			isFreshSessionIntent: () => harness.freshSessionIntent,
			closeCompletedSession: () => {},
			adoptDraftMessages: () => false,
			dispatchSession: (action) => harness.reducer.dispatch(action),
			getErroredSessionId: () => null,
			clearAskAwaiting: () => {},
			evictTerminalSessionMemory: () => {},
			clearStepBlockIds: () => {},
			flushChunksNow: () => {},
			updateSessionTitle: () => {},
			scheduleLoadSessions: () => {},
		});
		onLifecycle({
			event: 'session:lifecycle',
			id: 1,
			payload: {
				type: 'created',
				sessionId: CREATED_SESSION_ID,
				status: 'pending',
				waitingReason: null,
				title: null,
			},
		});
		expect(harness.reducer.snapshot().sessions).toContainEqual(
			expect.objectContaining({ sessionId: CREATED_SESSION_ID }),
		);
		expect(harness.reducer.snapshot().activeSessionId).toBeNull();
		expect(harness.freshSessionIntent).toBe(true);
	});

	it('keeps a new draft unselected when the real startup owner refreshes after an old end failure', async () => {
		let rejectEnd!: (error: Error) => void;
		const harness = makeHarness({
			runCommand: () =>
				new Promise((_, reject) => {
					rejectEnd = reject;
				}),
		});
		harness.reducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: OTHER_SESSION_ID,
			messages: [{ id: USER_MESSAGE_ID, role: 'user', content: 'Cached other session' }],
		});
		const startup = createChatSessionStartup({
			reducer: harness.reducer,
			dispatch: (action) => harness.reducer.dispatch(action),
			listRuntimeSessions: async () => ({
				sessions: [
					{
						sessionId: OTHER_SESSION_ID,
						status: 'paused',
						inputText: '',
						title: null,
						waitingReason: 'user_input',
					},
				],
			}),
			getLatestSessionForResume: async () => null,
			reopenSession: async () => {},
			refreshToolRuns: () => {},
			getFreshSessionIntent: () => harness.freshSessionIntent,
			setFreshSessionIntent: harness.setFreshSessionIntent,
			hasPersistedFreshSessionIntent: () => false,
			clearPersistedFreshSessionIntent: () => {},
			getPendingInteractionRequestIds: () => [],
			evictTerminalSessionMemory: () => {},
			setInitialLoading: () => {},
			deferResumeTargetClear: () => {},
			warn: () => {},
			reportError: () => {},
		});
		try {
			const ending = harness.controller.endSession();
			harness.controller.beginUserSessionSelection();
			harness.setFreshSessionIntent(true);
			harness.reducer.dispatch({ type: 'session/memory-cleared', sessionId: SESSION_ID });
			harness.reducer.dispatch({ type: 'session/cleared' });
			rejectEnd(new Error('late end failure'));
			await ending;
			await startup.loadSessions();
			expect(harness.reducer.snapshot().sessions).toHaveLength(1);
			expect(harness.reducer.snapshot().activeSessionId).toBeNull();
			expect(harness.freshSessionIntent).toBe(true);
		} finally {
			startup.dispose();
		}
	});

	it('does not clear a newer end intent after switching away and back', async () => {
		const endings: Array<{ resolve: () => void; reject: (error: Error) => void }> = [];
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'end_session'
					? new Promise<void>((resolve, reject) => {
							endings.push({ resolve, reject });
						})
					: resumeResponseFixture(),
		});
		const olderEnd = harness.controller.endSession();
		await harness.controller.switchToSession(OTHER_SESSION_ID);
		await harness.controller.switchToSession(SESSION_ID);
		expect(harness.freshSessionIntent).toBe(false);
		const newerEnd = harness.controller.endSession();
		endings[0]!.reject(new Error('old end failed'));
		await olderEnd;
		expect(harness.freshSessionIntent).toBe(true);
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		endings[1]!.resolve();
		await newerEnd;
	});

	it('does not clear fresh intent if lifecycle completion already cleared the active session', async () => {
		let rejectEnd!: (error: Error) => void;
		const harness = makeHarness({
			runCommand: () =>
				new Promise((_, reject) => {
					rejectEnd = reject;
				}),
		});
		const ending = harness.controller.endSession();
		// Completion closes the active session without beginning a user selection.
		harness.setFreshSessionIntent(true);
		harness.reducer.dispatch({ type: 'session/cleared' });
		rejectEnd(new Error('end failed after completion'));
		await ending;
		expect(harness.freshSessionIntent).toBe(true);
		expect(harness.reducer.snapshot().activeSessionId).toBeNull();
	});

	it('keeps the session selected when end fails and clears fresh-session intent', async () => {
		const error = new Error('end failed');
		const harness = makeHarness({ runCommand: () => Promise.reject(error) });

		await harness.controller.endSession();

		expect(harness.commandCalls).toEqual([
			{
				command: 'end_session',
				args: { sessionId: SESSION_ID },
			},
		]);
		expect(harness.freshSessionIntent).toBe(false);
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.reportedErrors).toEqual([{ error, message: '完成会话失败' }]);
	});

	it('clears the interrupt pending state and reports failure without success toast', async () => {
		const error = new Error('interrupt failed');
		const harness = makeHarness({ runCommand: () => Promise.reject(error) });

		await harness.controller.interruptOutput();

		expect(harness.commandCalls).toEqual([
			{
				command: 'interrupt_session',
				args: { sessionId: SESSION_ID },
			},
		]);
		expect(harness.interruptPending).toEqual([true, false]);
		expect(harness.reportedErrors).toEqual([{ error, message: '中断输出失败' }]);
		expect(harness.notifications).toEqual([]);
	});

	it('clears the previous terminal session only after a successful switch', async () => {
		const harness = makeHarness();

		const result = await harness.controller.switchToSession(OTHER_SESSION_ID);

		expect(result).toEqual({ kind: 'switched' });
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.reducer.getMessages(SESSION_ID)).toEqual([]);
		expect(harness.persistedIntentClears).toBe(1);
		expect(harness.notifications).toEqual([]);
	});

	it('ignores a stale resume result after a newer session selection starts', async () => {
		const resolvers: Array<(response: unknown) => void> = [];
		const harness = makeHarness({
			runCommand: (command) =>
				command === 'get_session_for_resume'
					? new Promise((resolve) => resolvers.push(resolve))
					: undefined,
		});

		const olderSwitch = harness.controller.switchToSession(OTHER_SESSION_ID);
		const newerSwitch = harness.controller.switchToSession(SESSION_ID);
		resolvers[0]?.(resumeResponseFixture());
		expect(await olderSwitch).toEqual({ kind: 'superseded' });
		expect(
			harness.actions.filter((action) => action.type === 'session/messages/resume-loaded'),
		).toEqual([]);

		resolvers[1]?.(resumeResponseFixture());
		expect(await newerSwitch).toEqual({ kind: 'switched' });
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(
			harness.actions
				.filter((action) => action.type === 'session/messages/resume-loaded')
				.map((action) => action.sessionId),
		).toEqual([SESSION_ID]);
	});
});
