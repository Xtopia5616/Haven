import { reportError, type ErrorReportOptions } from './errorHandling.ts';
import { addNotification, type NotificationType } from './notificationStore.ts';
import { buildResumeMessages } from './resumeMessages.ts';
import { getSessionForResume } from './sessionHistoryCommands.ts';
import {
	pickContinueStrategy,
	shouldResubmitOriginalUser,
	type ContinueStrategy,
} from './continueSession.ts';
import { isErrorStatus } from './sessionStatus.ts';
import { processResultSessionId } from './submit.ts';
import type { ChatFileAttachment, ChatImageAttachment } from './chatAttachmentTypes.ts';
import type { ProcessResult } from './contracts/generatedCommands.ts';
import type { RollbackSessionRequest, SessionIdRequest } from './contracts/commands.ts';
import type { TauriCommandInvoke } from './contracts/generatedCommands.ts';
import { resumeInteractions as resumeInteractionsFromProjection } from './sessionReducer.ts';
import type {
	SessionAction,
	SessionReducer,
	SessionSummary,
} from './sessionReducer.ts';

export interface RollbackRequest {
	stepNumber: number;
	role: string;
	content: string;
	msgId: string;
}

export interface ChatSessionControllerDependencies {
	invoke: TauriCommandInvoke;
	submitTranscript: (text: string, options: {
		images: ChatImageAttachment[] | null | undefined;
		files: ChatFileAttachment[] | null | undefined;
		reducer: SessionReducer;
		submissionToken?: string;
	}) => Promise<ProcessResult>;
	reducer: SessionReducer;
	dispatch: (action: SessionAction) => void;
	getActiveSessionId: () => string | null;
	getSessionSnapshot: () => SessionSummary[];
	notify: (message: string, type: NotificationType, duration?: number) => void;
	reportError: (error: unknown, options: ErrorReportOptions) => unknown;
	setInputDraft: (content: string) => void;
	loadSessions: () => Promise<void>;
	clearStepBlockIds: (sessionId: string) => void;
	setFreshSessionIntent: (value: boolean) => void;
	clearPersistedFreshSessionIntent: () => void;
	setRollbackLoading: (loading: boolean) => void;
	closeRollbackDialog: () => void;
	closeSessionMenu: () => void;
	setContinuePending: (pending: boolean) => void;
	setInterruptPending: (pending: boolean) => void;
	setAutoFollow: (follow: boolean) => void;
}

/** Owns chat session commands and the reducer transitions around them. */
export class ChatSessionController {
	private readonly dependencies: ChatSessionControllerDependencies;
	private rollbackInFlight = false;
	private continueInFlight = false;
	private interruptInFlight = false;

	constructor(dependencies: ChatSessionControllerDependencies) {
		this.dependencies = dependencies;
	}

	/** Read the pending interaction ids before applying a possibly stale snapshot. */
	pendingInteractionIdsForSession(sessionId: string): string[] {
		return this.dependencies.reducer.getPendingInteractionIds(sessionId);
	}

	evictTerminalSessionMemory(sessionId: string | null): void {
		if (!sessionId || sessionId === this.dependencies.getActiveSessionId()) return;
		this.dependencies.dispatch({ type: 'session/memory-cleared', sessionId });
	}

	/** Rebuild a session's in-memory messages from the authoritative resume projection. */
	async resyncSessionMessages(sessionId: string | null): Promise<void> {
		if (!sessionId) return;
		try {
			await this.reloadSessionMessages(sessionId, { preserveStreamingOnly: true });
		} catch (error) {
			this.report(error, '同步消息失败');
		}
	}

	async confirmRollbackAction(request: RollbackRequest): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId || this.rollbackInFlight) return;
		if (request.role === 'user' && !/^msg-[0-9a-f]{32}$/.test(request.msgId)) {
			this.dependencies.notify('消息仍在保存，请稍后再试', 'info', 2000);
			return;
		}

		this.rollbackInFlight = true;
		this.dependencies.setRollbackLoading(true);
		try {
			await this.dependencies.invoke('rollback_session', {
				sessionId,
				targetStep: request.stepNumber,
				pause: request.role === 'user',
				targetMessageId: request.msgId,
			} satisfies RollbackSessionRequest);
			this.dependencies.dispatch({ type: 'session/replay-reset', sessionId });
			this.dependencies.clearStepBlockIds(sessionId);
			await this.resyncSessionMessages(sessionId);

			if (request.role === 'user') {
				this.dependencies.setInputDraft(request.content);
				this.dependencies.notify('已回退，请编辑后重新发送', 'info', 3000);
			} else {
				this.dependencies.notify(`已回退到第 ${request.stepNumber} 步`, 'info', 3000);
			}
		} catch (error) {
			this.report(error, '回退失败');
		} finally {
			this.rollbackInFlight = false;
			this.dependencies.setRollbackLoading(false);
			this.dependencies.closeRollbackDialog();
			await this.dependencies.loadSessions();
		}
	}

	/** Load and select a persisted session, then reclaim terminal prior-session memory. */
	async switchToSession(sessionId: string): Promise<void> {
		this.dependencies.closeSessionMenu();
		const previousSessionId = this.dependencies.getActiveSessionId();
		try {
			await this.reloadSessionMessages(sessionId);
			this.dependencies.setFreshSessionIntent(false);
			this.dependencies.clearPersistedFreshSessionIntent();
			this.dependencies.dispatch({ type: 'session/selected', sessionId });

			if (previousSessionId && previousSessionId !== sessionId) {
				const previousSession = this.dependencies
					.getSessionSnapshot()
					.find((session) => session.id === previousSessionId);
				if (
					!previousSession ||
					previousSession.status === 'completed' ||
					isErrorStatus(previousSession.status)
				) {
					this.evictTerminalSessionMemory(previousSessionId);
				}
			}

			const session = this.dependencies
				.getSessionSnapshot()
				.find((item) => item.id === sessionId);
			this.dependencies.notify(`已切换到：${session?.title || '会话'}`, 'info', 1500);
		} catch (error) {
			this.report(error, '切换会话失败');
		}
	}

	async endSession(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId) return;
		// Prevent lifecycle events from auto-selecting an existing session while ending.
		this.dependencies.setFreshSessionIntent(true);
		try {
			await this.dependencies.invoke('end_session', { sessionId } satisfies SessionIdRequest);
		} catch (error) {
			// Keep the active pointer attached so a still-running session stays visible.
			this.dependencies.setFreshSessionIntent(false);
			this.report(error, '完成会话失败');
		}
	}

	async interruptOutput(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId || this.interruptInFlight) return;
		this.interruptInFlight = true;
		this.dependencies.setInterruptPending(true);
		try {
			await this.dependencies.invoke('interrupt_session', { sessionId } satisfies SessionIdRequest);
			this.dependencies.notify('输出已中断，可继续生成', 'info', 2000);
		} catch (error) {
			this.report(error, '中断输出失败');
		} finally {
			this.interruptInFlight = false;
			this.dependencies.setInterruptPending(false);
		}
	}

	async handleContinue(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId || this.continueInFlight) return;
		this.continueInFlight = true;
		this.dependencies.setContinuePending(true);

		const currentMessages = this.dependencies.reducer.getMessages(sessionId);
		// Capture identity before continue_session truncates partial output.
		const preContinueMessageIds = new Set(currentMessages.map((message) => message.id));
		const strategy: ContinueStrategy = pickContinueStrategy(currentMessages);
		try {
			await this.dependencies.invoke('continue_session', { sessionId } satisfies SessionIdRequest);
			this.dependencies.dispatch({ type: 'session/error-cleared', sessionId });

			// A failed reload does not prove that visible messages were partial.
			try {
				await this.reloadSessionMessages(sessionId, {
					preserveStreamingOnly: true,
					excludeMessageIds: [...preContinueMessageIds],
				});
			} catch {
				// Keep the current view until a later sync succeeds.
			}

			this.dependencies.dispatch({ type: 'session/replay-reset', sessionId });
			this.dependencies.setAutoFollow(true);
			if (strategy.mode === 'continue') {
				void this.submitMessage(strategy.text, []);
			} else {
				const syncedMessages = this.dependencies.reducer.getMessages(sessionId);
				if (shouldResubmitOriginalUser(syncedMessages, strategy.messageId)) {
					const submissionToken = strategy.messageId
						? `continue:${strategy.messageId}`
						: undefined;
					void this.submitMessage(strategy.text, [], undefined, submissionToken);
				}
			}
			await this.dependencies.loadSessions();
		} catch (error) {
			this.report(error, '继续失败');
			// Keep the banner visible so the user can retry.
		} finally {
			this.continueInFlight = false;
			this.dependencies.setContinuePending(false);
		}
	}

	/** Submit an input-router payload, keeping created-session selection in sync. */
	async submitMessage(
		text: string,
		images?: ChatImageAttachment[] | null,
		files?: ChatFileAttachment[] | null,
		submissionToken?: string,
	): Promise<void> {
		try {
			const result = await this.dependencies.submitTranscript(text, {
				images,
				files,
				reducer: this.dependencies.reducer,
				submissionToken,
			});
			const createdSessionId = processResultSessionId(result);
			if (createdSessionId) {
				this.dependencies.dispatch({
					type: 'session/selected',
					sessionId: createdSessionId,
				});
			}
			void this.dependencies.loadSessions();
		} catch (error) {
			this.report(error, '发送失败');
		}
	}

	private async reloadSessionMessages(
		sessionId: string,
		options: { preserveStreamingOnly?: boolean; excludeMessageIds?: string[] } = {},
	): Promise<void> {
		const result = await getSessionForResume({ sessionId }, this.dependencies.invoke);
		this.dependencies.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId,
			messages: buildResumeMessages(result),
			interactions: resumeInteractionsFromProjection(result),
			preserveInteractionIds: this.pendingInteractionIdsForSession(sessionId),
			usage: result.usage,
			llmUsage: result.llm_usage,
			...(options.preserveStreamingOnly ? { preserveStreamingOnly: true } : {}),
			...(options.excludeMessageIds
				? { excludeMessageIds: options.excludeMessageIds }
				: {}),
		});
	}

	private report(error: unknown, message: string): void {
		this.dependencies.reportError(error, { context: '+page', message, log: false });
	}
}

export function createChatSessionController(
	dependencies: ChatSessionControllerDependencies,
): ChatSessionController {
	return new ChatSessionController(dependencies);
}
