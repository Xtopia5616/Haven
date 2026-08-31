import logger from './logger.ts';
import {
	accumulateStreamChunk,
	finalizeStreamBlocks,
	type StreamMessage,
} from './streaming.ts';
import type { AgentChunkPayload } from './contracts/agent.ts';
import {
	pruneSeq,
	seqLastSeen,
	updateSessionMessages,
} from './sessionMessages.ts';

export interface PendingChunk {
	tid: string;
	sid: string;
	delta: string;
	msgType: string | undefined;
	stepNumber: number;
	runId: number;
	time: string;
	finalizeReasoning: boolean;
}

/**
 * Fold only adjacent chunks from one stream block. Keeping this as a pure
 * operation makes the ordering rule explicit and testable without coupling
 * it to the message store's reasoning-before-thought presentation policy.
 */
export function foldContiguousChunks(batch: readonly PendingChunk[]): PendingChunk[] {
	const merged: PendingChunk[] = [];
	for (const chunk of batch) {
		const previous = merged[merged.length - 1];
		if (
			previous &&
			previous.tid === chunk.tid &&
			previous.sid === chunk.sid &&
			previous.msgType === chunk.msgType &&
			previous.stepNumber === chunk.stepNumber &&
			previous.runId === chunk.runId
		) {
			previous.delta = (previous.delta || '') + (chunk.delta || '');
			previous.finalizeReasoning = previous.finalizeReasoning || chunk.finalizeReasoning;
		} else {
			merged.push({ ...chunk });
		}
	}
	return merged;
}

interface StepBlockIds {
	thoughtId?: string;
	reasoningId?: string;
}

interface StreamChunkEvent {
	payload: AgentChunkPayload;
}

export interface StreamEventAggregator {
	chunkHandler: (
		isThought: boolean,
		msgType: string | undefined,
	) => (event: StreamChunkEvent) => void;
	blockIdsOf: (sessionId: string, stepNumber: number, runId: number) => StepBlockIds;
	clearStepBlockIds: (sessionId: string | null) => void;
	flushChunksNow: () => void;
}

/**
 * Own the UI-side lifecycle of streamed thought/reasoning chunks.
 *
 * The page supplies only the active-session lookup and model-state callback.
 * Message mutation, frame batching, sequence dedupe, and step-block cleanup
 * stay together so authoritative thought/action/observation handlers can
 * synchronously flush this boundary before reading the message store.
 */
export function createStreamEventAggregator({
	getActiveSessionId,
	onActiveStream,
}: {
	getActiveSessionId: () => string | null;
	onActiveStream: () => void;
}): StreamEventAggregator {
	const pendingChunks: PendingChunk[] = [];
	let chunkFlushRaf = 0;
	const pendingChunkMax = 2000;
	let pendingChunkDrops = 0;
	const stepBlockIds = new Map<string, Map<string, StepBlockIds>>();

	function blockKey(stepNumber: number, runId: number) {
		return `${stepNumber}:${runId}`;
	}

	function registerBlockId(
		tid: string,
		stepNumber: number,
		runId: number,
		kind: 'thought' | 'reasoning',
		messageId: string,
	) {
		if (!tid || !messageId) return;
		let perSession = stepBlockIds.get(tid);
		if (!perSession) stepBlockIds.set(tid, (perSession = new Map()));
		const key = blockKey(stepNumber, runId);
		const entry = perSession.get(key) || {};
		if (kind === 'thought') entry.thoughtId = messageId;
		else entry.reasoningId = messageId;
		perSession.set(key, entry);
	}

	function blockIdsOf(sessionId: string, stepNumber: number, runId: number) {
		return stepBlockIds.get(sessionId)?.get(blockKey(stepNumber, runId)) || {};
	}

	function clearStepBlockIds(sessionId: string | null) {
		if (!sessionId) return;
		const perSession = stepBlockIds.get(sessionId);
		if (perSession) {
			for (const { thoughtId, reasoningId } of perSession.values()) {
				if (thoughtId) pruneSeq(thoughtId);
				if (reasoningId) pruneSeq(reasoningId);
			}
		}
		stepBlockIds.delete(sessionId);
	}

	function flushPendingChunks() {
		chunkFlushRaf = 0;
		if (pendingChunks.length === 0) return;
		const batch = pendingChunks.splice(0);
		// Merge only contiguous chunks from the same stream block. A map keyed by
		// message id is tempting, but it silently moves A₂ next to A₁ when the
		// arrival order is A₁, B₁, A₂ (common around reasoning/search boundaries).
		// That turns ordering into text corruption. Contiguous folding keeps the
		// O(steps × list) batching benefit without inventing a new order.
		const merged = foldContiguousChunks(batch);
		// Group by session while preserving arrival order within each session.
		const bySession = new Map<string, PendingChunk[]>();
		for (const chunk of merged) {
			let list = bySession.get(chunk.tid);
			if (!list) bySession.set(chunk.tid, (list = []));
			list.push(chunk);
		}
		for (const [sessionId, chunks] of bySession) {
			updateSessionMessages(sessionId, (messages) => {
				let next: StreamMessage[] = messages;
				for (const chunk of chunks) {
					if (chunk.finalizeReasoning) {
						const { reasoningId } = blockIdsOf(chunk.tid, chunk.stepNumber, chunk.runId);
						if (reasoningId) {
							next = finalizeStreamBlocks(next, reasoningId, null);
							pruneSeq(reasoningId);
						}
					}
					if (chunk.delta) {
						next = accumulateStreamChunk(next, {
							messageId: chunk.sid,
							delta: chunk.delta,
							msgType: chunk.msgType,
							stepNumber: chunk.stepNumber,
							runId: chunk.runId,
							time: chunk.time,
						});
					}
				}
				return next;
			});
		}
	}

	function flushChunksNow() {
		if (chunkFlushRaf) {
			cancelAnimationFrame(chunkFlushRaf);
			chunkFlushRaf = 0;
		}
		flushPendingChunks();
	}

	function chunkHandler(isThought: boolean, msgType: string | undefined) {
		return (event: StreamChunkEvent) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			const messageId = data.messageId;
			const delta = data.delta || '';
			if (getActiveSessionId() === sessionId) onActiveStream();
			if (seqLastSeen(messageId, data.seq, sessionId)) return;
			registerBlockId(
				sessionId,
				data.stepNumber,
				data.runId,
				isThought ? 'thought' : 'reasoning',
				messageId,
			);
			pendingChunks.push({
				tid: sessionId,
				sid: messageId,
				delta,
				msgType,
				stepNumber: data.stepNumber,
				runId: data.runId,
				time: new Date().toLocaleTimeString(),
				finalizeReasoning: isThought,
			});
			if (pendingChunks.length > pendingChunkMax) {
				pendingChunks.shift();
				pendingChunkDrops++;
				if (pendingChunkDrops === 1) {
					logger.warn(
						'streamAggregator',
						`chunk queue overflow (${pendingChunkMax}), evicting oldest chunks`,
					);
				}
			}
			if (!chunkFlushRaf) {
				chunkFlushRaf = requestAnimationFrame(flushPendingChunks);
			}
		};
	}

	return { chunkHandler, blockIdsOf, clearStepBlockIds, flushChunksNow };
}
