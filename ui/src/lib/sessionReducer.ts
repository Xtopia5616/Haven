import { writable, type Writable } from 'svelte/store';
import { reduceAgent } from './sessionReducer/agent.ts';
import { reduceInteraction } from './sessionReducer/interaction.ts';
import { reduceLifecycle } from './sessionReducer/lifecycle.ts';
import { reduceTranscript } from './sessionReducer/transcript.ts';
import { reduceUsage } from './sessionReducer/usage.ts';
import { blockIdsOf, messagesOf } from './sessionReducer/state.ts';
import { createEqualityGatedSessionSelectorStore } from './sessionReducer/selectorStore.ts';
import { initialSessionState } from './sessionReducer/types.ts';
import type {
	SessionAction,
	SessionMessage,
	SessionReducerState,
	StreamBlockIds,
} from './sessionReducer/types.ts';

export { DRAFT_SESSION_ID, initialSessionState } from './sessionReducer/types.ts';
export type {
	AgentChunkBatchItem,
	ResumeUsage,
	SessionAction,
	SessionMessage,
	PendingMessageOwner,
	SessionReducerState,
	SessionReplayState,
	SessionSummary,
	SessionRunEndNotice,
	SessionRunEndStatus,
	SessionTokenStats,
	StreamBlockIds,
} from './sessionReducer/types.ts';
export { backgroundToolRunResultContent } from './sessionReducer/transcript.ts';

function clearSessionMessages(state: SessionReducerState, sessionId: string): SessionReducerState {
	const withoutMessages = reduceTranscript(state, {
		type: 'session/messages/cleared',
		sessionId,
	});
	const pendingMessageOwners = Object.fromEntries(
		Object.entries(withoutMessages.pendingMessageOwners).filter(
			([, message]) => message.sessionId !== sessionId,
		),
	);
	const withoutPendingMessageOwners = { ...withoutMessages, pendingMessageOwners };
	const withoutInteractions = reduceInteraction(withoutPendingMessageOwners, {
		type: 'session/interactions-cleared',
		sessionId,
	});
	return reduceUsage(withoutInteractions, {
		type: 'session/usage-cleared',
		sessionId,
	});
}

function clearSessionWorkingDocument(
	state: SessionReducerState,
	sessionId: string,
): SessionReducerState {
	const workingDocuments = { ...state.workingDocuments };
	const workingDocumentSequences = { ...state.workingDocumentSequences };
	const workingDocumentEventWatermarks = { ...state.workingDocumentEventWatermarks };
	delete workingDocuments[sessionId];
	delete workingDocumentSequences[sessionId];
	delete workingDocumentEventWatermarks[sessionId];
	return {
		...state,
		workingDocuments,
		workingDocumentSequences,
		workingDocumentEventWatermarks,
	};
}

function resumeSession(
	state: SessionReducerState,
	action: Extract<SessionAction, { type: 'session/messages/resume-loaded' }>,
): SessionReducerState {
	let next = reduceTranscript(state, action);
	const resumedMessages = next.messages[action.sessionId] || [];
	const liveMessages = resumedMessages.filter((message) => !message.toolCallPreview);
	if (liveMessages.length !== resumedMessages.length) {
		const chunkSeqByMessage = { ...next.replay.chunkSeqByMessage };
		for (const message of resumedMessages) {
			if (message.toolCallPreview) delete chunkSeqByMessage[message.id];
		}
		next = {
			...next,
			messages: { ...next.messages, [action.sessionId]: liveMessages },
			replay: { ...next.replay, chunkSeqByMessage },
		};
	}
	if (action.interactions) {
		next = reduceInteraction(next, {
			type: 'session/interactions-hydrated',
			sessionId: action.sessionId,
			requests: action.interactions,
			preserveRequestIds: action.preserveRequestIds,
		});
	}
	if (action.usage || action.llmUsage) {
		next = reduceUsage(next, {
			type: 'session/usage-restored',
			sessionId: action.sessionId,
			usage: action.usage,
			llmUsage: action.llmUsage,
		});
	}
	const currentWorkingDocumentWatermark =
		next.workingDocumentEventWatermarks[action.sessionId] ?? -1;
	const snapshotEventWatermark =
		action.eventSequenceWatermark ?? action.workingDocumentSequence ?? -1;
	const canApplyWorkingDocumentSnapshot =
		snapshotEventWatermark >= currentWorkingDocumentWatermark;
	if (
		Object.prototype.hasOwnProperty.call(action, 'workingDocument') &&
		canApplyWorkingDocumentSnapshot
	) {
		const workingDocuments = { ...next.workingDocuments };
		if (action.workingDocument) workingDocuments[action.sessionId] = action.workingDocument;
		else delete workingDocuments[action.sessionId];
		const workingDocumentSequences = { ...next.workingDocumentSequences };
		if (action.workingDocumentSequence != null) {
			workingDocumentSequences[action.sessionId] = action.workingDocumentSequence;
		} else {
			delete workingDocumentSequences[action.sessionId];
		}
		const workingDocumentEventWatermarks = { ...next.workingDocumentEventWatermarks };
		if (snapshotEventWatermark >= 0) {
			workingDocumentEventWatermarks[action.sessionId] = snapshotEventWatermark;
		}
		next = {
			...next,
			workingDocuments,
			workingDocumentSequences,
			workingDocumentEventWatermarks,
		};
	}
	return next;
}

function updateWorkingDocument(
	state: SessionReducerState,
	action: Extract<SessionAction, { type: 'session/working-document-updated' }>,
): SessionReducerState {
	if (
		action.eventSequence <=
		(state.replay.resumeSnapshotWatermarkBySession[action.sessionId] ?? 0)
	)
		return state;
	if (
		action.eventSequence <=
		(state.workingDocumentEventWatermarks[action.sessionId] ?? -1)
	)
		return state;
	const workingDocuments = { ...state.workingDocuments };
	if (action.markdown) workingDocuments[action.sessionId] = action.markdown;
	else delete workingDocuments[action.sessionId];
	return {
		...state,
		workingDocuments,
		workingDocumentSequences: {
			...state.workingDocumentSequences,
			[action.sessionId]: action.eventSequence,
		},
		workingDocumentEventWatermarks: {
			...state.workingDocumentEventWatermarks,
			[action.sessionId]: action.eventSequence,
		},
		replay: {
			...state.replay,
			eventSequenceWatermarkBySession: {
				...state.replay.eventSequenceWatermarkBySession,
				[action.sessionId]: Math.max(
					state.replay.eventSequenceWatermarkBySession[action.sessionId] ?? 0,
					action.eventSequence,
				),
			},
			eventSequenceGapsBySession: {
				...state.replay.eventSequenceGapsBySession,
				...(action.eventSequence >
				(state.replay.eventSequenceWatermarkBySession[action.sessionId] ?? 0) + 1
					? { [action.sessionId]: true }
					: {}),
			},
		},
	};
}

/** Route the public action union to its private domain transition. */
export function reduceSession(
	inputState: SessionReducerState,
	action: SessionAction,
): SessionReducerState {
	switch (action.type) {
		case 'sessions/loaded':
		case 'session/created':
		case 'session/selected':
		case 'session/cleared':
		case 'session/status-updated':
		case 'session/run-end-notice-cleared':
		case 'session/error-reason-remembered':
		case 'session/error-reason-forgotten':
		case 'session/run-ended':
		case 'session/retained-error':
		case 'session/title-updated':
			return reduceLifecycle(inputState, action);

		case 'sessions/cleared': {
			const withoutTranscript = reduceTranscript(inputState, action);
			const withoutInteractions = reduceInteraction(withoutTranscript, action);
			const withoutUsage = reduceUsage(withoutInteractions, action);
			const withoutWorkingDocuments = {
				...withoutUsage,
				workingDocuments: {},
				workingDocumentSequences: {},
				workingDocumentEventWatermarks: {},
			};
			return reduceLifecycle(withoutWorkingDocuments, action);
		}

		case 'session/deleted':
			if (!action.sessionId) return reduceSession(inputState, { type: 'sessions/cleared' });
			return reduceLifecycle(
				clearSessionWorkingDocument(
					clearSessionMessages(inputState, action.sessionId),
					action.sessionId,
				),
				action,
			);

		case 'session/messages/cleared':
			return clearSessionMessages(inputState, action.sessionId);

		case 'session/memory-cleared':
			return clearSessionWorkingDocument(
				clearSessionMessages(inputState, action.sessionId),
				action.sessionId,
			);

		case 'session/asks-settled':
			return reduceInteraction(reduceTranscript(inputState, action), action);

		case 'session/messages/resume-loaded':
			if (
				action.eventSequenceWatermark != null &&
				action.eventSequenceWatermark <
					(inputState.replay.eventSequenceWatermarkBySession[action.sessionId] ?? 0)
			)
				return inputState;
			{
				const resumed = resumeSession(inputState, action);
				if (action.eventSequenceWatermark == null) return resumed;
				return {
					...resumed,
					replay: {
						...resumed.replay,
						eventSequenceWatermarkBySession: {
							...resumed.replay.eventSequenceWatermarkBySession,
							[action.sessionId]: Math.max(
								resumed.replay.eventSequenceWatermarkBySession[action.sessionId] ?? 0,
								action.eventSequenceWatermark,
							),
						},
						resumeSnapshotWatermarkBySession: {
							...resumed.replay.resumeSnapshotWatermarkBySession,
							[action.sessionId]: Math.max(
								resumed.replay.resumeSnapshotWatermarkBySession[action.sessionId] ?? 0,
								action.eventSequenceWatermark,
							),
						},
						eventSequenceGapsBySession: {
							...resumed.replay.eventSequenceGapsBySession,
							[action.sessionId]: false,
						},
					},
				};
			}

		case 'session/working-document-updated':
			return updateWorkingDocument(inputState, action);

		case 'session/messages/optimistic-added':
		case 'session/messages/accepted':
		case 'session/messages/rejected':
		case 'session/messages/finalized':
		case 'session/messages/adopt-draft':
		case 'session/messages/truncated':
		case 'session/replay-reset':
		case 'session/stream-blocks-cleared':
		case 'session/background-result':
			return reduceTranscript(inputState, action);

		case 'session/interaction-upserted':
		case 'session/interactions-hydrated':
		case 'session/interactions-cleared':
		case 'session/ask-resolved':
		case 'session/interaction-resolution-result':
		case 'session/schedule-run-cancelled':
			return reduceInteraction(inputState, action);

		case 'agent/chunks':
		case 'agent/tool-call-chunk':
		case 'agent/thought':
		case 'agent/stream-reset':
		case 'agent/web-search':
		case 'agent/supplement':
		case 'agent/tool_call':
		case 'agent/observation':
			return reduceAgent(inputState, action);

		case 'session/usage-restored':
		case 'session/usage-live':
		case 'session/usage-cleared':
			return reduceUsage(inputState, action);
	}
	return inputState;
}

type SessionStateListener = (state: SessionReducerState) => void;

/** Observable wrapper used by the route and reducer-focused tests. */
export class SessionReducer {
	private state: SessionReducerState;
	private userSelectionVersion = 0;
	private readonly resumeSnapshotVersions = new Map<string, number>();
	private readonly pendingResumeSnapshots = new Map<string, number>();
	private readonly listeners = new Set<SessionStateListener>();
	private readonly stateStore?: Writable<SessionReducerState>;

	constructor(
		initialState: SessionReducerState = initialSessionState,
		stateStore?: Writable<SessionReducerState>,
	) {
		this.state = initialState;
		this.stateStore = stateStore;
	}

	snapshot(): SessionReducerState {
		return this.state;
	}

	/** Mark an explicit user selection intent before any asynchronous work begins. */
	beginUserSelection(): number {
		this.userSelectionVersion += 1;
		return this.userSelectionVersion;
	}

	currentUserSelectionVersion(): number {
		return this.userSelectionVersion;
	}

	isUserSelectionCurrent(version: number): boolean {
		return this.userSelectionVersion === version;
	}

	getMessages(sessionId: string): SessionMessage[] {
		return messagesOf(this.state, sessionId);
	}

	getEventSequenceWatermark(sessionId: string): number {
		return this.state.replay.eventSequenceWatermarkBySession[sessionId] ?? 0;
	}

	needsEventReconciliation(sessionId: string): boolean {
		return this.state.replay.eventSequenceGapsBySession[sessionId] === true;
	}

	beginResumeSnapshot(sessionId: string): number {
		this.pendingResumeSnapshots.set(
			sessionId,
			(this.pendingResumeSnapshots.get(sessionId) ?? 0) + 1,
		);
		return this.resumeSnapshotVersions.get(sessionId) ?? 0;
	}

	isResumeSnapshotVersionCurrent(sessionId: string, version: number): boolean {
		return (this.resumeSnapshotVersions.get(sessionId) ?? 0) === version;
	}

	finishResumeSnapshot(sessionId: string): void {
		const remaining = (this.pendingResumeSnapshots.get(sessionId) ?? 1) - 1;
		if (remaining > 0) {
			this.pendingResumeSnapshots.set(sessionId, remaining);
			return;
		}
		this.pendingResumeSnapshots.delete(sessionId);
		this.resumeSnapshotVersions.delete(sessionId);
	}

	/** Invalidate snapshots that started before a timeline-mutating command. */
	invalidateResumeSnapshots(sessionId: string): void {
		if (!this.pendingResumeSnapshots.has(sessionId)) return;
		this.resumeSnapshotVersions.set(
			sessionId,
			(this.resumeSnapshotVersions.get(sessionId) ?? 0) + 1,
		);
	}

	getPendingInteractionRequestIds(sessionId: string): string[] {
		return Object.values(this.state.interactions || {})
			.filter(
				(request) =>
					request.owner.kind === 'session' &&
					request.owner.sessionId === sessionId &&
					request.status === 'pending',
			)
			.map((request) => request.requestId);
	}

	getSessionErrorReason(sessionId: string): string {
		return this.state.sessionErrorReasons[sessionId] || '';
	}

	getBlockIds(sessionId: string, stepNumber: number, runId: number): StreamBlockIds {
		return blockIdsOf(this.state, sessionId, stepNumber, runId);
	}

	dispatch(action: SessionAction): SessionReducerState {
		if (
			action.type === 'session/messages/truncated' ||
			action.type === 'session/replay-reset' ||
			action.type === 'session/messages/cleared' ||
			action.type === 'session/memory-cleared'
		) {
			this.invalidateResumeSnapshots(action.sessionId);
		} else if (action.type === 'session/deleted' && action.sessionId) {
			this.invalidateResumeSnapshots(action.sessionId);
		} else if (action.type === 'session/cleared' || action.type === 'sessions/cleared') {
			for (const sessionId of new Set([
				...Object.keys(this.state.messages),
				...this.pendingResumeSnapshots.keys(),
			])) {
				this.invalidateResumeSnapshots(sessionId);
			}
		}
		const next = reduceSession(this.state, action);
		if (next === this.state) return this.state;
		this.state = next;
		this.stateStore?.set(next);
		for (const listener of this.listeners) listener(this.state);
		return this.state;
	}

	subscribe(listener: SessionStateListener): () => void {
		this.listeners.add(listener);
		listener(this.state);
		return () => this.listeners.delete(listener);
	}
}

/** The one application-wide reducer shared by chat, history and the shell. */
export const sessionStateStore = writable<SessionReducerState>(initialSessionState);
export const appSessionReducer = new SessionReducer(initialSessionState, sessionStateStore);

/** Read a reducer-owned slice without notifying consumers for unrelated updates. */
export function createSessionSelectorStore<T>(
	select: (state: SessionReducerState) => T,
	equals: (previous: T, next: T) => boolean = Object.is,
) {
	return createEqualityGatedSessionSelectorStore(sessionStateStore, select, equals);
}
