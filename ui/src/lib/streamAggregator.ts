import logger from './logger.ts';
import type { AgentChunkPayload } from './contracts/agent.ts';
import type { AgentChunkBatchItem, SessionAction } from './sessionReducer.ts';

export interface PendingChunk {
	tid: string;
	sid: string;
	delta: string;
	msgType: string | undefined;
	stepNumber: number;
	runId: number;
	seq?: number;
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
			previous.seq = chunk.seq ?? previous.seq;
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
	metricsSnapshot: () => StreamMetricsSnapshot;
}

export interface StreamMetricsSnapshot {
	frames: number;
	chunks: number;
	drops: number;
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
	dispatch,
	getBlockIds,
}: {
	getActiveSessionId: () => string | null;
	onActiveStream: () => void;
	dispatch: (action: SessionAction) => void;
	getBlockIds: (sessionId: string, stepNumber: number, runId: number) => StepBlockIds;
}): StreamEventAggregator {
	const pendingChunks: PendingChunk[] = [];
	let chunkFlushRaf = 0;
	const firstChunkPainted = new Set<string>();
	const firstChunkPaintedOrder: string[] = [];
	const firstChunkPaintedMax = 512;
	const pendingChunkMax = 2000;
	let pendingChunkDrops = 0;
	let frameCount = 0;
	let acceptedChunkCount = 0;
	function blockIdsOf(sessionId: string, stepNumber: number, runId: number) {
		return getBlockIds(sessionId, stepNumber, runId);
	}

	function clearStepBlockIds(sessionId: string | null) {
		if (!sessionId) return;
		const sessionPrefix = `${sessionId}:`;
		for (const key of firstChunkPainted) {
			if (key.startsWith(sessionPrefix)) firstChunkPainted.delete(key);
		}
		for (let index = firstChunkPaintedOrder.length - 1; index >= 0; index--) {
			if (firstChunkPaintedOrder[index].startsWith(sessionPrefix)) {
				firstChunkPaintedOrder.splice(index, 1);
			}
		}
		dispatch({ type: 'session/stream-blocks-cleared', sessionId });
	}

	function flushPendingChunks(fromAnimationFrame = false) {
		chunkFlushRaf = 0;
		if (pendingChunks.length === 0) return;
		const batch = pendingChunks.splice(0);
		// Merge only contiguous chunks from the same stream block. A map keyed by
		// message id is tempting, but it silently moves A₂ next to A₁ when the
		// arrival order is A₁, B₁, A₂ (common around reasoning/search boundaries).
		// That turns ordering into text corruption. Contiguous folding keeps the
		// O(steps × list) batching benefit without inventing a new order.
		const merged = foldContiguousChunks(batch);
		// Only a real requestAnimationFrame callback is a renderer frame. Manual
		// flushes are synchronization boundaries used before teardown or an
		// authoritative event handler and must not inflate the RAF metric.
		if (fromAnimationFrame) frameCount++;
		// Group by session while preserving arrival order within each session.
		const bySession = new Map<string, PendingChunk[]>();
		for (const chunk of merged) {
			let list = bySession.get(chunk.tid);
			if (!list) bySession.set(chunk.tid, (list = []));
			list.push(chunk);
		}
		for (const [, chunks] of bySession) {
			const frame: AgentChunkBatchItem[] = chunks.map((chunk) => ({
				kind: chunk.msgType === undefined ? 'thought' : 'reasoning',
				...(chunk.msgType !== undefined ? { msgType: chunk.msgType } : {}),
				payload: {
					sessionId: chunk.tid,
					delta: chunk.delta,
					stepNumber: chunk.stepNumber,
					runId: chunk.runId,
					messageId: chunk.sid,
					seq: chunk.seq ?? 0,
				},
			}));
			dispatch({ type: 'agent/chunks', chunks: frame });
		}
	}

	function flushChunksNow() {
		if (chunkFlushRaf) {
			cancelAnimationFrame(chunkFlushRaf);
			chunkFlushRaf = 0;
		}
		flushPendingChunks();
	}

	function flushFirstChunkNow() {
		// A previous stream block may already have scheduled a renderer frame.
		// Cancel it before the immediate first-paint flush; otherwise that stale
		// callback can race the next RAF and steal the next delta from its frame.
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
			pendingChunks.push({
				tid: sessionId,
				sid: messageId,
				delta,
				msgType,
				stepNumber: data.stepNumber,
				runId: data.runId,
				seq: data.seq,
				time: new Date().toLocaleTimeString(),
				finalizeReasoning: isThought,
			});
			acceptedChunkCount++;
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
			// Paint the first visible delta immediately. Later chunks still share
			// one animation frame, keeping the steady-state render cost bounded
			// without adding a frame of latency to the first byte.
			const streamKey = `${sessionId}:${data.stepNumber}:${data.runId}`;
			if (delta && getActiveSessionId() === sessionId && !firstChunkPainted.has(streamKey)) {
				firstChunkPainted.add(streamKey);
				firstChunkPaintedOrder.push(streamKey);
				if (firstChunkPaintedOrder.length > firstChunkPaintedMax) {
					const evicted = firstChunkPaintedOrder.shift();
					if (evicted) firstChunkPainted.delete(evicted);
				}
				flushFirstChunkNow();
				return;
			}
			if (!chunkFlushRaf) {
				chunkFlushRaf = requestAnimationFrame(() => flushPendingChunks(true));
			}
		};
	}

	return {
		chunkHandler,
		blockIdsOf,
		clearStepBlockIds,
		flushChunksNow,
		metricsSnapshot: () => ({
			frames: frameCount,
			chunks: acceptedChunkCount,
			drops: pendingChunkDrops,
		}),
	};
}
