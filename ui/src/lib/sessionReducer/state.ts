import type {
	SessionMessage,
	SessionReducerState,
	SessionReplayState,
	StreamBlockIds,
} from './types.ts';

const EVENT_SEQ_HISTORY_LIMIT = 4096;

export function messagesOf(state: SessionReducerState, sessionId: string): SessionMessage[] {
	return state.messages[sessionId] || [];
}

export function withMessages(
	state: SessionReducerState,
	sessionId: string,
	fn: (messages: SessionMessage[]) => SessionMessage[],
): SessionReducerState {
	const current = messagesOf(state, sessionId);
	const next = fn(current);
	if (next === current) return state;
	return { ...state, messages: { ...state.messages, [sessionId]: next } };
}

export function replayOf(state: SessionReducerState): SessionReplayState {
	return state.replay;
}

export function streamBlockKey(stepNumber: number, runId: number): string {
	return `${stepNumber}:${runId}`;
}

export function blockIdsOf(
	state: SessionReducerState,
	sessionId: string,
	stepNumber: number,
	runId: number,
): StreamBlockIds {
	return replayOf(state).blockIdsBySession[sessionId]?.[streamBlockKey(stepNumber, runId)] || {};
}

export function registerBlock(
	state: SessionReducerState,
	sessionId: string,
	stepNumber: number,
	runId: number,
	kind: 'thought' | 'reasoning',
	messageId: string,
): SessionReducerState {
	if (!sessionId || !messageId) return state;
	const replay = replayOf(state);
	const sessionBlocks = replay.blockIdsBySession[sessionId] || {};
	const key = streamBlockKey(stepNumber, runId);
	const entry = sessionBlocks[key] || {};
	const field = kind === 'thought' ? 'thoughtId' : 'reasoningId';
	if (entry[field] === messageId) return state;
	return {
		...state,
		replay: {
			...replay,
			blockIdsBySession: {
				...replay.blockIdsBySession,
				[sessionId]: { ...sessionBlocks, [key]: { ...entry, [field]: messageId } },
			},
		},
	};
}

/** Return null for a duplicate durable card. Parallel ToolCalls share one sequence, so the key is eventSequence plus the card identity. A missing sequence is not durable and must not dedup. */
export function acceptEventSequence(
	state: SessionReducerState,
	sessionId: string,
	eventSequence: number | undefined,
	identity: string,
): SessionReducerState | null {
	if (eventSequence == null || !Number.isFinite(eventSequence) || !identity) return state;
	const replay = replayOf(state);
	if (eventSequence <= (replay.resumeSnapshotWatermarkBySession[sessionId] ?? 0)) return null;
	const previous = replay.eventSequenceBySession[sessionId] || [];
	const key = `${eventSequence}:${identity}`;
	if (previous.includes(key)) return null;
	const seen = [...previous, key];
	if (seen.length > EVENT_SEQ_HISTORY_LIMIT)
		seen.splice(0, seen.length - EVENT_SEQ_HISTORY_LIMIT);
	return {
		...state,
		replay: {
			...replay,
			eventSequenceBySession: { ...replay.eventSequenceBySession, [sessionId]: seen },
			eventSequenceWatermarkBySession: {
				...replay.eventSequenceWatermarkBySession,
				[sessionId]: Math.max(replay.eventSequenceWatermarkBySession[sessionId] ?? 0, eventSequence),
			},
			eventSequenceGapsBySession: {
				...replay.eventSequenceGapsBySession,
				...(eventSequence > (replay.eventSequenceWatermarkBySession[sessionId] ?? 0) + 1
					? { [sessionId]: true }
					: {}),
			},
		},
	};
}

export function clearReplayForSession(
	state: SessionReducerState,
	sessionId: string,
	preserveEventSequence = false,
): SessionReducerState {
	const replay = replayOf(state);
	const eventSequenceBySession = { ...replay.eventSequenceBySession };
	const eventSequenceWatermarkBySession = { ...replay.eventSequenceWatermarkBySession };
	const resumeSnapshotWatermarkBySession = { ...replay.resumeSnapshotWatermarkBySession };
	const eventSequenceGapsBySession = { ...replay.eventSequenceGapsBySession };
	const blockIdsBySession = { ...replay.blockIdsBySession };
	const chunkSeqByMessage = { ...replay.chunkSeqByMessage };
	if (!preserveEventSequence) {
		delete eventSequenceBySession[sessionId];
		delete eventSequenceWatermarkBySession[sessionId];
		delete resumeSnapshotWatermarkBySession[sessionId];
		delete eventSequenceGapsBySession[sessionId];
	} else {
		resumeSnapshotWatermarkBySession[sessionId] = Math.max(
			resumeSnapshotWatermarkBySession[sessionId] ?? 0,
			eventSequenceWatermarkBySession[sessionId] ?? 0,
		);
	}
	for (const ids of Object.values(blockIdsBySession[sessionId] || {})) {
		if (ids.thoughtId) delete chunkSeqByMessage[ids.thoughtId];
		if (ids.reasoningId) delete chunkSeqByMessage[ids.reasoningId];
	}
	delete blockIdsBySession[sessionId];
	for (const message of messagesOf(state, sessionId)) delete chunkSeqByMessage[message.id];
	return {
		...state,
		replay: {
			...replay,
			eventSequenceBySession,
			eventSequenceWatermarkBySession,
			resumeSnapshotWatermarkBySession,
			eventSequenceGapsBySession,
			blockIdsBySession,
			chunkSeqByMessage,
		},
	};
}
