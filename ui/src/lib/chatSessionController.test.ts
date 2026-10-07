import { describe, expect, it } from 'vitest';
import {
	createChatSessionController,
	type ChatSessionControllerDependencies,
} from './chatSessionController.ts';
import type { InteractionRequest } from './contracts/app.ts';
import type { SessionResumeInput } from './contracts/sessionHistory.ts';
import { SessionReducer, type SessionAction } from './sessionReducer.ts';
import type { ProcessResult } from './contracts/generatedCommands.ts';

const SESSION_ID = 'ses-00000000000000000000000000000001';
const OTHER_SESSION_ID = 'ses-00000000000000000000000000000002';
const USER_MESSAGE_ID = 'msg-00000000000000000000000000000001';

function sessionResumeInput(overrides: Partial<SessionResumeInput> = {}): SessionResumeInput {
	return {
		session: { id: SESSION_ID, status: 'paused' },
		messages: [],
		steps: [],
		usage: null,
		llm_usage: [],
		interactions: [],
		...overrides,
	};
}

function makeHarness(options: {
	invoke?: (command: string, args?: unknown) => unknown | Promise<unknown>;
	submit?: (
		text: string,
		args: Parameters<ChatSessionControllerDependencies['submitTranscript']>[1],
	) => ProcessResult | Promise<ProcessResult>;
} = {}) {
	const reducer = new SessionReducer();
	reducer.dispatch({
		type: 'sessions/loaded',
		sessions: [
			{ id: SESSION_ID, status: 'error', title: '旧会话' },
			{ id: OTHER_SESSION_ID, status: 'paused', title: '目标会话' },
		],
	});
	reducer.dispatch({ type: 'session/selected', sessionId: SESSION_ID });

	const invokeCalls: Array<{ command: string; args?: unknown }> = [];
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

	const dependencies: ChatSessionControllerDependencies = {
		invoke: async <T = unknown>(command: string, args?: unknown): Promise<T> => {
			invokeCalls.push({ command, args });
			const result = options.invoke
				? await options.invoke(command, args)
				: command === 'get_session_for_resume'
					? sessionResumeInput()
					: undefined;
			return result as T;
		},
		submitTranscript: async (text, args) => {
			submitCalls.push({ text, args });
			return options.submit ? await options.submit(text, args) : { Supplemented: {} };
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
		closeRollbackDialog: () => rollbackDialogClosed++,
		closeSessionMenu: () => sessionMenuClosed++,
		setContinuePending: (pending) => continuePending.push(pending),
		setInterruptPending: (pending) => interruptPending.push(pending),
		setAutoFollow: (follow) => autoFollow.push(follow),
	};

	return {
		controller: createChatSessionController(dependencies),
		reducer,
		invokeCalls,
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

function addMessages(reducer: SessionReducer, ...messages: Array<{ id: string; role: string; content: string; type?: string }>) {
	reducer.dispatch({
		type: 'session/messages/resume-loaded',
		sessionId: SESSION_ID,
		messages,
	});
}

function pendingInteraction(sessionId = SESSION_ID): InteractionRequest {
	return {
		id: 'conf-pending',
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
			role: 'user',
			content: '重新编辑这段',
			msgId: USER_MESSAGE_ID,
		});

		expect(harness.invokeCalls[0]).toEqual({
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
			role: 'assistant',
			content: '',
			msgId: 'step-00000000000000000000000000000007',
		});

		expect(harness.invokeCalls[0]?.args).toEqual({
			sessionId: SESSION_ID,
			targetStep: 7,
			pause: false,
			targetMessageId: 'step-00000000000000000000000000000007',
		});
		expect(harness.inputDrafts).toEqual([]);
		expect(harness.notifications[0]?.message).toBe('已回退到第 7 步');
	});
});

describe('ChatSessionController continue', () => {
	it('sends 继续 after generated assistant output and keeps the authoritative reload ordered', async () => {
		const harness = makeHarness();
		addMessages(
			harness.reducer,
			{ id: USER_MESSAGE_ID, role: 'user', content: '写一首诗' },
			{ id: 'step-thought', role: 'assistant', content: '已有一段', type: 'thought' },
		);

		await harness.controller.handleContinue();

		expect(harness.invokeCalls.map((call) => call.command)).toEqual([
			'continue_session',
			'get_session_for_resume',
		]);
		expect(harness.invokeCalls[0]?.args).toEqual({ sessionId: SESSION_ID });
		expect(harness.submitCalls.map((call) => call.text)).toEqual(['继续']);
		expect(harness.actions.map((action) => action.type)).toContain('session/error-cleared');
		expect(harness.actions.map((action) => action.type)).toContain('session/replay-reset');
		expect(harness.autoFollow).toEqual([true]);
		expect(harness.continuePending).toEqual([true, false]);
		// The submitted follow-up and the continue path each refresh sessions,
		// matching the prior page orchestration order.
		expect(harness.loadSessionsCalls).toBe(2);
	});

	it('resubmits the original user turn when it did not survive the resume reload', async () => {
		const harness = makeHarness({
			invoke: (command) =>
				command === 'get_session_for_resume' ? sessionResumeInput() : undefined,
		});
		addMessages(harness.reducer, {
			id: USER_MESSAGE_ID,
			role: 'user',
			content: '打开计算器',
		});

		await harness.controller.handleContinue();

		expect(harness.submitCalls.map((call) => call.text)).toEqual(['打开计算器']);
	});

	it('does not resubmit an original user turn still present in the authoritative snapshot', async () => {
		const harness = makeHarness({
			invoke: (command) => command === 'get_session_for_resume'
				? sessionResumeInput({
						messages: [{
							id: USER_MESSAGE_ID,
							role: 'user',
							content: '打开计算器',
							created_at: '2026-09-25T00:00:00Z',
						}],
					})
				: undefined,
		});
		addMessages(harness.reducer, {
			id: USER_MESSAGE_ID,
			role: 'user',
			content: '打开计算器',
		});

		await harness.controller.handleContinue();

		expect(harness.submitCalls).toEqual([]);
	});

	it('preserves pending interactions when a resume snapshot contains none', async () => {
		const harness = makeHarness();
		harness.reducer.dispatch({ type: 'session/interaction-upserted', request: pendingInteraction() });

		await harness.controller.resyncSessionMessages(SESSION_ID);

		expect(harness.reducer.snapshot().interactions['conf-pending']?.status).toBe('pending');
		const resumeAction = harness.actions.find(
			(action) => action.type === 'session/messages/resume-loaded',
		);
		expect(resumeAction).toMatchObject({ preserveInteractionIds: ['conf-pending'] });
	});

	it('ignores a duplicate continue request while the first command is in flight', async () => {
		let finishContinue: (() => void) | undefined;
		const continueCommand = new Promise<void>((resolve) => {
			finishContinue = resolve;
		});
		const harness = makeHarness({
			invoke: (command) =>
				command === 'continue_session' ? continueCommand : sessionResumeInput(),
		});

		const first = harness.controller.handleContinue();
		await harness.controller.handleContinue();
		expect(harness.invokeCalls.filter((call) => call.command === 'continue_session')).toHaveLength(1);
		finishContinue?.();
		await first;
	});

	it('ignores a duplicate rollback while the first command is in flight', async () => {
		let finishRollback: (() => void) | undefined;
		const rollbackCommand = new Promise<void>((resolve) => {
			finishRollback = resolve;
		});
		const harness = makeHarness({
			invoke: (command) =>
				command === 'rollback_session' ? rollbackCommand : sessionResumeInput(),
		});
		const request = {
			stepNumber: 7,
			role: 'assistant',
			content: '',
			msgId: 'step-00000000000000000000000000000007',
		};

		const first = harness.controller.confirmRollbackAction(request);
		await harness.controller.confirmRollbackAction(request);

		expect(harness.invokeCalls.filter((call) => call.command === 'rollback_session')).toHaveLength(1);
		expect(harness.invokeCalls[0]?.args).toEqual({
			sessionId: SESSION_ID,
			targetStep: 7,
			pause: false,
			targetMessageId: request.msgId,
		});
		finishRollback?.();
		await first;
	});
});

describe('ChatSessionController session guards', () => {
	it('selects a newly created session after submitTranscript resolves', async () => {
		const harness = makeHarness({
			submit: () => ({ SessionCreated: { session_id: OTHER_SESSION_ID } }),
		});

		await harness.controller.submitMessage('新会话第一条消息');

		expect(harness.reducer.snapshot().activeSessionId).toBe(OTHER_SESSION_ID);
		expect(harness.loadSessionsCalls).toBe(1);
	});

	it('keeps the session selected when end fails and clears fresh-session intent', async () => {
		const error = new Error('end failed');
		const harness = makeHarness({ invoke: () => Promise.reject(error) });

		await harness.controller.endSession();

		expect(harness.invokeCalls).toEqual([{
			command: 'end_session',
			args: { sessionId: SESSION_ID },
		}]);
		expect(harness.freshSessionIntent).toBe(false);
		expect(harness.reducer.snapshot().activeSessionId).toBe(SESSION_ID);
		expect(harness.reportedErrors).toEqual([{ error, message: '完成会话失败' }]);
	});

	it('clears the interrupt pending state and reports failure without success toast', async () => {
		const error = new Error('interrupt failed');
		const harness = makeHarness({ invoke: () => Promise.reject(error) });

		await harness.controller.interruptOutput();

		expect(harness.invokeCalls).toEqual([{
			command: 'interrupt_session',
			args: { sessionId: SESSION_ID },
		}]);
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
});
