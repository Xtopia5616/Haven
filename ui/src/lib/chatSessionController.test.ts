import { describe, expect, it } from 'vitest';
import {
	createChatSessionController,
	type ChatSessionControllerDependencies,
} from './chatSessionController.ts';
import type { InteractionRequest } from './contracts/app.ts';
import { SessionReducer, type SessionAction } from './sessionReducer.ts';
import type { TranscriptSubmissionResultView } from './contracts/transcriptSubmissionResult.ts';
import type { CanonicalRole } from './contracts/generatedCommands.ts';
import type { SessionResumeResponse } from './contracts/sessionHistory.ts';
import type { SessionMessagePresentationType } from './streaming.ts';
import type { ResumeTranscriptProjectionInput } from './resumeMessages.ts';

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
	const inputDrafts: string[] = [];
	const continuePending: boolean[] = [];
	const interruptPending: boolean[] = [];
	const rollbackLoading: boolean[] = [];
	const autoFollow: boolean[] = [];
	let freshSessionIntent = false;
	let persistedIntentClears = 0;
	let rollbackDialogClosed = 0;
	let sessionMenuClosed = 0;
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
		setInputDraft: (content) => inputDrafts.push(content),
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
		closeForkDialog: () => {},
		closeRollbackDialog: () => rollbackDialogClosed++,
		closeSessionMenu: () => sessionMenuClosed++,
		setContinuePending: (pending) => continuePending.push(pending),
		setInterruptPending: (pending) => interruptPending.push(pending),
		setAutoFollow: (follow) => autoFollow.push(follow),
	};

	return {
		controller: createChatSessionController(dependencies),
		reducer,
		commandCalls,
		submitCalls,
		actions,
		notifications,
		reportedErrors,
		stepBlockClears,
		inputDrafts,
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
		get rollbackDialogClosed() {
			return rollbackDialogClosed;
		},
		get sessionMenuClosed() {
			return sessionMenuClosed;
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

describe('ChatSessionController rollback', () => {
	it('pauses and restores the draft when rolling back a persisted user message', async () => {
		const harness = makeHarness();
		await harness.controller.confirmRollbackAction({
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
		expect(harness.actions.map((action) => action.type)).toContain('session/replay-reset');
		expect(harness.stepBlockClears).toEqual([SESSION_ID]);
		expect(harness.inputDrafts).toEqual(['重新编辑这段']);
		expect(harness.notifications).toContainEqual({
			message: '已回退，请编辑后重新发送',
			type: 'info',
			duration: 3000,
		});
		expect(harness.rollbackLoading).toEqual([true, false]);
		expect(harness.rollbackDialogClosed).toBe(1);
		expect(harness.loadSessionsCalls).toBe(1);
	});

	it('keeps an action rollback running and reports its step number', async () => {
		const harness = makeHarness();
		await harness.controller.confirmRollbackAction({
			stepNumber: 7,
			role: 'assistant' as const,
			content: '',
			messageId: 'step-00000000000000000000000000000007',
		});

		expect(harness.commandCalls[0]?.args).toEqual({
			sessionId: SESSION_ID,
			targetStep: 7,
			pause: false,
			targetMessageId: null,
		});
		expect(harness.inputDrafts).toEqual([]);
		expect(harness.notifications[0]?.message).toBe('已回退到第 7 步');
	});

	it('does not send a timeline step identity as a rollback message target', async () => {
		const harness = makeHarness();
		await harness.controller.confirmRollbackAction({
			stepNumber: 8,
			role: 'user' as const,
			content: '正在保存',
			messageId: 'step-00000000000000000000000000000008',
		});

		expect(harness.commandCalls).toEqual([]);
		expect(harness.notifications[0]?.message).toBe('消息仍在保存，请稍后再试');
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
		// The submitted follow-up and the continue path each refresh sessions,
		// matching the prior page orchestration order.
		expect(harness.loadSessionsCalls).toBe(2);
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

	it('preserves pending interactions when a resume snapshot contains none', async () => {
		const harness = makeHarness();
		harness.reducer.dispatch({
			type: 'session/interaction-upserted',
			request: pendingInteraction(),
		});

		await harness.controller.resyncSessionMessages(SESSION_ID);

		expect(harness.reducer.snapshot().interactions['step-aac817798dc536ab69caa990732988f1']?.status).toBe('pending');
		const resumeAction = harness.actions.find(
			(action) => action.type === 'session/messages/resume-loaded',
		);
		expect(resumeAction).toMatchObject({ preserveRequestIds: ['step-aac817798dc536ab69caa990732988f1'] });
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
		await harness.controller.confirmRollbackAction(request);

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
		await pendingRollback;

		expect(harness.inputDrafts).toEqual([]);
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

		await harness.controller.submitMessage('新会话第一条消息');

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.loadSessionsCalls).toBe(1);
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
		await pendingSubmit;

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
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

		await harness.controller.switchToSession(OTHER_SESSION_ID);

		expect(harness.sessionMenuClosed).toBe(1);
		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.reducer.getMessages(SESSION_ID)).toEqual([]);
		expect(harness.persistedIntentClears).toBe(1);
		expect(harness.notifications[0]?.message).toBe('已切换到：目标会话');
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
		expect(await olderSwitch).toBe(false);
		expect(
			harness.actions.filter((action) => action.type === 'session/messages/resume-loaded'),
		).toEqual([]);

		resolvers[1]?.(resumeResponseFixture());
		expect(await newerSwitch).toBe(true);
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(
			harness.actions
				.filter((action) => action.type === 'session/messages/resume-loaded')
				.map((action) => action.sessionId),
		).toEqual([SESSION_ID]);
	});
});
