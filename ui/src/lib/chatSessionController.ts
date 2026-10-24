import type { InputDeliveryModeInput } from './contracts/generatedCommands.ts';
import { reportError, type ErrorReportOptions } from './errorHandling.ts';
import { addNotification, type NotificationType } from './notificationStore.ts';
import { buildResumeMessages } from './resumeMessages.ts';
import type * as sessionCommandAdapter from './sessionCommands.ts';
import {
	pickContinueStrategy,
	shouldResubmitOriginalUser,
	type ContinueStrategy,
} from './continueSession.ts';
import { isErrorStatus } from './sessionStatus.ts';
import { transcriptSubmissionResultSessionId } from './submit.ts';
import type { ChatAttachmentPayload, ChatFileAttachment } from './chatAttachmentTypes.ts';
import type { CanonicalRole } from './contracts/generatedCommands.ts';
import type { TranscriptSubmissionResultView } from './contracts/transcriptSubmissionResult.ts';
import { isCanonicalEntityId } from './contracts/wireGuards.ts';
import type { SessionAction, SessionReducer, SessionSummary } from './sessionReducer.ts';

export interface RollbackRequest {
	stepNumber: number;
	role: CanonicalRole;
	content: string;
	messageId: string;
}

export interface ChatSessionControllerDependencies {
	commands: Pick<
		typeof sessionCommandAdapter,
		| 'continueSession'
		| 'endSession'
		| 'forkSessionFromMessage'
		| 'getSessionEventSequenceWatermark'
		| 'getSessionForResume'
		| 'interruptSession'
		| 'rollbackSession'
	>;
	submitTranscript: (
		text: string,
		options: {
			images: ChatAttachmentPayload[] | null | undefined;
			files: ChatFileAttachment[] | null | undefined;
			reducer: SessionReducer;
			submissionToken?: string;
			deliveryMode?: InputDeliveryModeInput;
		},
	) => Promise<TranscriptSubmissionResultView>;
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
	setForkLoading: (loading: boolean) => void;
	closeForkDialog: () => void;
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
	private forkInFlight = false;
	private lastReconciliationErrorAt = 0;

	constructor(dependencies: ChatSessionControllerDependencies) {
		this.dependencies = dependencies;
	}

	/** Read the pending interaction ids before applying a possibly stale snapshot. */
	pendingInteractionRequestIdsForSession(sessionId: string): string[] {
		return this.dependencies.reducer.getPendingInteractionRequestIds(sessionId);
	}

	beginUserSessionSelection(): number {
		return this.dependencies.reducer.beginUserSelection();
	}

	isUserSessionSelectionCurrent(version: number): boolean {
		return this.dependencies.reducer.isUserSelectionCurrent(version);
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

	/** Repair committed transcript events that the renderer did not receive. */
	async reconcileSessionEvents(sessionId: string | null): Promise<void> {
		if (!sessionId || this.rollbackInFlight || this.continueInFlight) return;
		const snapshotVersion = this.dependencies.reducer.beginResumeSnapshot(sessionId);
		try {
			const watermarkBefore = await this.dependencies.commands.getSessionEventSequenceWatermark({
				sessionId,
			});
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(sessionId, snapshotVersion)
			)
				return;
			const knownWatermark = this.dependencies.reducer.getEventSequenceWatermark(sessionId);
			if (
				watermarkBefore <= knownWatermark &&
				!this.dependencies.reducer.needsEventReconciliation(sessionId)
			)
				return;

			const result = await this.dependencies.commands.getSessionForResume({ sessionId });
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(sessionId, snapshotVersion)
			)
				return;
			const watermarkAfter = await this.dependencies.commands.getSessionEventSequenceWatermark({
				sessionId,
			});
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(sessionId, snapshotVersion) ||
				watermarkBefore !== watermarkAfter ||
				result.eventSequenceWatermark !== watermarkAfter
			)
				return;
			if (
				watermarkAfter <= this.dependencies.reducer.getEventSequenceWatermark(sessionId) &&
				!this.dependencies.reducer.needsEventReconciliation(sessionId)
			)
				return;

			this.applyResumeSnapshot(sessionId, result, { preserveStreamingOnly: true });
		} catch (error) {
			const now = Date.now();
			if (now - this.lastReconciliationErrorAt >= 30_000) {
				this.lastReconciliationErrorAt = now;
				this.report(error, '自动同步消息失败');
			}
		} finally {
			this.dependencies.reducer.finishResumeSnapshot(sessionId);
		}
	}

	async confirmRollbackAction(request: RollbackRequest): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId || this.rollbackInFlight) return;
		const userSelectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		if (request.role === 'user' && !isCanonicalEntityId(request.messageId, 'msg')) {
			this.dependencies.notify('消息仍在保存，请稍后再试', 'info', 2000);
			return;
		}

		this.rollbackInFlight = true;
		this.dependencies.reducer.invalidateResumeSnapshots(sessionId);
		this.dependencies.setRollbackLoading(true);
		try {
			await this.dependencies.commands.rollbackSession({
				sessionId,
				targetStep: request.stepNumber,
				pause: request.role === 'user',
				targetMessageId: request.role === 'user' ? request.messageId : null,
			});
			this.dependencies.dispatch({ type: 'session/replay-reset', sessionId });
			this.dependencies.clearStepBlockIds(sessionId);
			await this.resyncSessionMessages(sessionId);

			if (
				this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) &&
				this.dependencies.getActiveSessionId() === sessionId &&
				request.role === 'user'
			) {
				this.dependencies.setInputDraft(request.content);
				this.dependencies.notify('已回退，请编辑后重新发送', 'info', 3000);
			} else if (
				this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) &&
				this.dependencies.getActiveSessionId() === sessionId
			) {
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

	/** Create and select an independent child session from one user message. */
	async forkSessionFromMessage(messageId: string): Promise<void> {
		const sourceSessionId = this.dependencies.getActiveSessionId();
		if (!sourceSessionId || this.forkInFlight) return;
		if (!isCanonicalEntityId(messageId, 'msg')) {
			this.dependencies.notify('消息仍在保存，请稍后再试', 'info', 2000);
			return;
		}

		const selectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		this.forkInFlight = true;
		this.dependencies.setForkLoading(true);
		try {
			const childSessionId = await this.dependencies.commands.forkSessionFromMessage({
				sessionId: sourceSessionId,
				messageId,
			});
			await this.dependencies.loadSessions();
			if (
				this.dependencies.reducer.isUserSelectionCurrent(selectionVersion) &&
				this.dependencies.getActiveSessionId() === sourceSessionId
			) {
				await this.switchToSession(childSessionId, selectionVersion);
			} else {
				this.dependencies.notify('已创建派生会话', 'info', 2000);
			}
		} catch (error) {
			this.report(error, '创建派生会话失败');
		} finally {
			this.forkInFlight = false;
			this.dependencies.setForkLoading(false);
			this.dependencies.closeForkDialog();
		}
	}

	/** Load and select a persisted session, then reclaim terminal prior-session memory. */
	async switchToSession(sessionId: string, selectionVersion?: number): Promise<boolean> {
		const currentSelectionVersion = selectionVersion ?? this.beginUserSessionSelection();
		this.dependencies.closeSessionMenu();
		const previousSessionId = this.dependencies.getActiveSessionId();
		try {
			const loaded = await this.reloadSessionMessages(sessionId, {
				selectionVersion: currentSelectionVersion,
			});
			if (!loaded || !this.isUserSessionSelectionCurrent(currentSelectionVersion))
				return false;
			this.dependencies.setFreshSessionIntent(false);
			this.dependencies.clearPersistedFreshSessionIntent();
			this.dependencies.dispatch({ type: 'session/selected', sessionId });

			if (previousSessionId && previousSessionId !== sessionId) {
				const previousSession = this.dependencies
					.getSessionSnapshot()
					.find((session) => session.sessionId === previousSessionId);
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
				.find((item) => item.sessionId === sessionId);
			this.dependencies.notify(`已切换到：${session?.title || '会话'}`, 'info', 1500);
			return true;
		} catch (error) {
			if (this.isUserSessionSelectionCurrent(currentSelectionVersion)) {
				this.report(error, '切换会话失败');
			}
			return false;
		}
	}

	async endSession(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId) return;
		// Prevent lifecycle events from auto-selecting an existing session while ending.
		this.dependencies.setFreshSessionIntent(true);
		try {
			await this.dependencies.commands.endSession({ sessionId });
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
			await this.dependencies.commands.interruptSession({ sessionId });
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
		const userSelectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		this.continueInFlight = true;
		this.dependencies.reducer.invalidateResumeSnapshots(sessionId);
		this.dependencies.setContinuePending(true);

		const currentMessages = this.dependencies.reducer.getMessages(sessionId);
		// Capture identity before continue_session truncates partial output.
		const preContinueMessageIds = new Set(currentMessages.map((message) => message.id));
		const strategy: ContinueStrategy = pickContinueStrategy(currentMessages);
		try {
			await this.dependencies.commands.continueSession({ sessionId });
			if (
				!this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) ||
				this.dependencies.getActiveSessionId() !== sessionId
			) {
				return;
			}
			this.dependencies.dispatch({ type: 'session/run-end-notice-cleared', sessionId });

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

	/** Submit a Composer payload, keeping created-session selection in sync. */
	async submitMessage(
		text: string,
		images?: ChatAttachmentPayload[] | null,
		files?: ChatFileAttachment[] | null,
		submissionToken?: string,
		deliveryMode?: InputDeliveryModeInput,
	): Promise<void> {
		const userSelectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		try {
			const result = await this.dependencies.submitTranscript(text, {
				images,
				files,
				reducer: this.dependencies.reducer,
				submissionToken,
				...(deliveryMode ? { deliveryMode } : {}),
			});
			const createdSessionId = transcriptSubmissionResultSessionId(result);
			if (
				createdSessionId &&
				this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) &&
				this.dependencies.getActiveSessionId() !== createdSessionId
			) {
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
		options: {
			preserveStreamingOnly?: boolean;
			excludeMessageIds?: string[];
			selectionVersion?: number;
		} = {},
	): Promise<boolean> {
		const snapshotVersion = this.dependencies.reducer.beginResumeSnapshot(sessionId);
		try {
			const result = await this.dependencies.commands.getSessionForResume({ sessionId });
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(sessionId, snapshotVersion) ||
				(options.selectionVersion != null &&
					!this.isUserSessionSelectionCurrent(options.selectionVersion))
			) {
				return false;
			}
			this.applyResumeSnapshot(sessionId, result, options);
			return true;
		} finally {
			this.dependencies.reducer.finishResumeSnapshot(sessionId);
		}
	}

	private applyResumeSnapshot(
		sessionId: string,
		result: Awaited<ReturnType<ChatSessionControllerDependencies['commands']['getSessionForResume']>>,
		options: { preserveStreamingOnly?: boolean; excludeMessageIds?: string[] },
	): void {
		this.dependencies.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId,
			messages: buildResumeMessages(result),
			interactions: result.interactions,
			preserveRequestIds: this.pendingInteractionRequestIdsForSession(sessionId),
			usage: result.usage,
			llmUsage: result.llm_usage,
			workingDocument: result.workingDocument,
			workingDocumentSequence: result.workingDocumentSequence,
			eventSequenceWatermark: result.eventSequenceWatermark,
			...(options.preserveStreamingOnly ? { preserveStreamingOnly: true } : {}),
			...(options.excludeMessageIds ? { excludeMessageIds: options.excludeMessageIds } : {}),
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
