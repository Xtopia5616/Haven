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
import type { ChatSessionSubmitResult } from './chatSessionOperationResults.ts';

export interface RollbackRequest {
	stepNumber: number;
	role: CanonicalRole;
	content: string;
	messageId: string;
}

export type RollbackResult =
	| { kind: 'ignored'; reason: 'no-active-session' | 'already-in-flight' }
	| { kind: 'rejected'; reason: 'message-not-persisted' }
	| { kind: 'failed'; sessionId: string; error: unknown }
	| {
			kind: 'rolled-back';
			sessionId: string;
			stepNumber: number;
			role: CanonicalRole;
			content: string;
			selectionCurrent: boolean;
			transcriptRefresh: RollbackTranscriptRefresh;
	  };

type RollbackTranscriptRefresh = 'loaded' | 'superseded' | { kind: 'failed'; error: unknown };

export type SessionSwitchResult =
	{ kind: 'switched' } | { kind: 'superseded' } | { kind: 'failed'; error: unknown };

export type SessionForkResult =
	| { kind: 'ignored'; reason: 'no-active-session' | 'already-in-flight' }
	| { kind: 'rejected'; reason: 'message-not-persisted' }
	| { kind: 'failed'; error: unknown }
	| { kind: 'created'; sessionId: string; sourceSessionId: string; selectionVersion: number };

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
	loadSessions: () => Promise<void>;
	clearStepBlockIds: (sessionId: string) => void;
	setFreshSessionIntent: (value: boolean) => void;
	clearPersistedFreshSessionIntent: () => void;
	setRollbackLoading: (loading: boolean) => void;
	setForkLoading: (loading: boolean) => void;
	setContinuePending: (pending: boolean) => void;
	setInterruptPendingSessions: (sessionIds: readonly string[]) => void;
	setAutoFollow: (follow: boolean) => void;
}

/** Owns chat session commands and the reducer transitions around them. */
export class ChatSessionController {
	private readonly dependencies: ChatSessionControllerDependencies;
	private rollbackInFlight = false;
	private continueInFlight = false;
	private readonly interruptPendingSessions = new Set<string>();
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
			const watermarkBefore =
				await this.dependencies.commands.getSessionEventSequenceWatermark({
					sessionId,
				});
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(
					sessionId,
					snapshotVersion,
				)
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
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(
					sessionId,
					snapshotVersion,
				)
			)
				return;
			const watermarkAfter =
				await this.dependencies.commands.getSessionEventSequenceWatermark({
					sessionId,
				});
			if (
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(
					sessionId,
					snapshotVersion,
				) ||
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

	async confirmRollbackAction(request: RollbackRequest): Promise<RollbackResult> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId) return { kind: 'ignored', reason: 'no-active-session' };
		if (this.rollbackInFlight) return { kind: 'ignored', reason: 'already-in-flight' };
		const userSelectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		if (request.role === 'user' && !isCanonicalEntityId(request.messageId, 'msg')) {
			return { kind: 'rejected', reason: 'message-not-persisted' };
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
			let transcriptRefresh: RollbackTranscriptRefresh = 'loaded';
			try {
				const loaded = await this.reloadSessionMessages(sessionId, {
					preserveStreamingOnly: true,
				});
				transcriptRefresh = loaded ? 'loaded' : 'superseded';
			} catch (error) {
				transcriptRefresh = { kind: 'failed', error };
			}
			return {
				kind: 'rolled-back',
				sessionId,
				stepNumber: request.stepNumber,
				role: request.role,
				content: request.content,
				selectionCurrent:
					this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) &&
					this.dependencies.getActiveSessionId() === sessionId,
				transcriptRefresh,
			};
		} catch (error) {
			return { kind: 'failed', sessionId, error };
		} finally {
			this.rollbackInFlight = false;
			this.dependencies.setRollbackLoading(false);
		}
	}

	/** Create a child and capture the selection intent for the later page refresh. */
	async forkSessionFromMessage(messageId: string): Promise<SessionForkResult> {
		const sourceSessionId = this.dependencies.getActiveSessionId();
		if (!sourceSessionId) return { kind: 'ignored', reason: 'no-active-session' };
		if (this.forkInFlight) return { kind: 'ignored', reason: 'already-in-flight' };
		if (!isCanonicalEntityId(messageId, 'msg')) {
			return { kind: 'rejected', reason: 'message-not-persisted' };
		}

		const selectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		this.forkInFlight = true;
		this.dependencies.setForkLoading(true);
		try {
			const childSessionId = await this.dependencies.commands.forkSessionFromMessage({
				sessionId: sourceSessionId,
				messageId,
			});
			return {
				kind: 'created',
				sessionId: childSessionId,
				sourceSessionId,
				selectionVersion,
			};
		} catch (error) {
			return { kind: 'failed', error };
		} finally {
			this.forkInFlight = false;
			this.dependencies.setForkLoading(false);
		}
	}

	async selectForkedSession(
		fork: Extract<SessionForkResult, { kind: 'created' }>,
	): Promise<SessionSwitchResult> {
		if (
			!this.isUserSessionSelectionCurrent(fork.selectionVersion) ||
			this.dependencies.getActiveSessionId() !== fork.sourceSessionId
		)
			return { kind: 'superseded' };
		return this.switchToSession(fork.sessionId, fork.selectionVersion);
	}

	/** Load and select a persisted session, then reclaim terminal prior-session memory. */
	async switchToSession(
		sessionId: string,
		selectionVersion?: number,
	): Promise<SessionSwitchResult> {
		const currentSelectionVersion = selectionVersion ?? this.beginUserSessionSelection();
		const previousSessionId = this.dependencies.getActiveSessionId();
		try {
			const loaded = await this.reloadSessionMessages(sessionId, {
				selectionVersion: currentSelectionVersion,
			});
			if (!loaded || !this.isUserSessionSelectionCurrent(currentSelectionVersion))
				return { kind: 'superseded' };
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

			return { kind: 'switched' };
		} catch (error) {
			return this.isUserSessionSelectionCurrent(currentSelectionVersion)
				? { kind: 'failed', error }
				: { kind: 'superseded' };
		}
	}

	async endSession(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId) return;
		const selectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		// Prevent lifecycle events from auto-selecting an existing session while ending.
		this.dependencies.setFreshSessionIntent(true);
		try {
			await this.dependencies.commands.endSession({ sessionId });
		} catch (error) {
			// Restore only the intent owned by this selection. A newer draft or
			// session choice must survive a late failure from the previous session.
			if (
				this.dependencies.reducer.isUserSelectionCurrent(selectionVersion) &&
				this.dependencies.getActiveSessionId() === sessionId
			)
				this.dependencies.setFreshSessionIntent(false);
			this.report(error, '完成会话失败');
		}
	}

	async interruptOutput(): Promise<void> {
		const sessionId = this.dependencies.getActiveSessionId();
		if (!sessionId || this.interruptPendingSessions.has(sessionId)) return;
		const selectionVersion = this.dependencies.reducer.currentUserSelectionVersion();
		this.interruptPendingSessions.add(sessionId);
		this.dependencies.setInterruptPendingSessions([...this.interruptPendingSessions]);
		try {
			await this.dependencies.commands.interruptSession({ sessionId });
			if (
				this.dependencies.reducer.isUserSelectionCurrent(selectionVersion) &&
				this.dependencies.getActiveSessionId() === sessionId
			)
				this.dependencies.notify('输出已中断，可继续生成', 'info', 2000);
		} catch (error) {
			this.report(error, '中断输出失败');
		} finally {
			this.interruptPendingSessions.delete(sessionId);
			this.dependencies.setInterruptPendingSessions([...this.interruptPendingSessions]);
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
					selectionVersion: userSelectionVersion,
					preserveStreamingOnly: true,
					excludeMessageIds: [...preContinueMessageIds],
				});
			} catch {
				// Keep the current view until a later sync succeeds.
			}

			// Reload can settle after a newer selection, even when it fails.
			if (
				!this.dependencies.reducer.isUserSelectionCurrent(userSelectionVersion) ||
				this.dependencies.getActiveSessionId() !== sessionId
			) {
				return;
			}

			this.dependencies.dispatch({ type: 'session/replay-reset', sessionId });
			this.dependencies.setAutoFollow(true);
			if (strategy.mode === 'continue') {
				const submission = await this.submitMessage(strategy.text, []);
				if (submission.kind === 'failed') this.report(submission.error, '发送失败');
			} else {
				const syncedMessages = this.dependencies.reducer.getMessages(sessionId);
				if (shouldResubmitOriginalUser(syncedMessages, strategy.messageId)) {
					const submissionToken = strategy.messageId
						? `continue:${strategy.messageId}`
						: undefined;
					const submission = await this.submitMessage(
						strategy.text,
						[],
						undefined,
						submissionToken,
					);
					if (submission.kind === 'failed') this.report(submission.error, '发送失败');
				}
			}
			try {
				await this.dependencies.loadSessions();
			} catch (error) {
				this.report(error, '刷新会话列表失败');
			}
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
	): Promise<ChatSessionSubmitResult> {
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
			return { kind: 'submitted', sessionId: createdSessionId ?? null };
		} catch (error) {
			return { kind: 'failed', error };
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
				!this.dependencies.reducer.isResumeSnapshotVersionCurrent(
					sessionId,
					snapshotVersion,
				) ||
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
		result: Awaited<
			ReturnType<ChatSessionControllerDependencies['commands']['getSessionForResume']>
		>,
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
