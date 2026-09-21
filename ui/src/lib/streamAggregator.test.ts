import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AgentChunkPayload } from './contracts/agent.ts';
import { SessionReducer } from './sessionReducer.ts';
import {
	createStreamEventAggregator,
	foldContiguousChunks,
	type PendingChunk,
} from './streamAggregator.ts';

describe('createStreamEventAggregator', () => {
	let frame: FrameRequestCallback | null = null;
	let reducer: SessionReducer;

	beforeEach(() => {
		reducer = new SessionReducer();
		frame = null;
		vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
			frame = callback;
			return 1;
		});
		vi.stubGlobal('cancelAnimationFrame', vi.fn());
	});

	function createAggregator(
		dispatch: (action: import('./sessionReducer.ts').SessionAction) => void = (action) =>
			reducer.dispatch(action),
	) {
		return createStreamEventAggregator({
			getActiveSessionId: () => 'ses-stream-test',
			onActiveStream: vi.fn(),
			dispatch,
			getBlockIds: (sessionId, stepNumber, runId) =>
				reducer.getBlockIds(sessionId, stepNumber, runId),
		});
	}

	function chunk(overrides: Partial<AgentChunkPayload> = {}): { payload: AgentChunkPayload } {
		return {
			payload: {
				sessionId: 'ses-stream-test',
				delta: 'hello',
				stepNumber: 1,
				runId: 1,
				messageId: 'step-thought-test',
				seq: 1,
				...overrides,
			},
		};
	}

	it('leaves sequence replay handling to the reducer', () => {
		const aggregator = createAggregator();
		const handler = aggregator.chunkHandler(true, undefined);

		handler(chunk());
		handler(chunk({ delta: 'duplicate', seq: 1 }));
		expect(reducer.getMessages('ses-stream-test')).toHaveLength(1);
		expect(reducer.getMessages('ses-stream-test')[0].content).toBe('hello');

		aggregator.flushChunksNow();
		expect(reducer.getMessages('ses-stream-test')).toHaveLength(1);
		expect(reducer.getMessages('ses-stream-test')[0].content).toBe('hello');
		expect(aggregator.metricsSnapshot()).toEqual({ frames: 0, chunks: 2, drops: 0 });
	});

	it('dispatches a thought frame after the reducer finalizes reasoning', () => {
		const aggregator = createAggregator();
		aggregator.chunkHandler(
			false,
			'reasoning',
		)(chunk({ messageId: 'step-reasoning-test', delta: 'reason', seq: 1 }));
		aggregator.chunkHandler(
			true,
			undefined,
		)(chunk({ messageId: 'step-thought-test-2', delta: 'thought', seq: 1 }));

		aggregator.flushChunksNow();
		const messages = reducer.getMessages('ses-stream-test');
		expect(messages.map((message) => [message.content, message.streaming])).toEqual([
			['reason', false],
			['thought', true],
		]);
	});

	it('keeps interleaved stream blocks in arrival order', () => {
		const base: PendingChunk = {
			tid: 'ses-stream-test',
			sid: 'step-thought-a',
			delta: '',
			msgType: undefined,
			stepNumber: 1,
			runId: 1,
			time: 'now',
			finalizeReasoning: true,
		};
		const folded = foldContiguousChunks([
			{ ...base, delta: 'A1' },
			{ ...base, sid: 'step-reasoning-b', delta: 'B1', finalizeReasoning: false },
			{ ...base, delta: 'A2' },
		]);

		expect(folded.map(({ sid, delta }) => [sid, delta])).toEqual([
			['step-thought-a', 'A1'],
			['step-reasoning-b', 'B1'],
			['step-thought-a', 'A2'],
		]);
	});

	it('dispatches one reducer action for each session frame while retaining chunk order', () => {
		const dispatch = vi.fn();
		const aggregator = createAggregator(dispatch);
		aggregator.chunkHandler(true, undefined)(chunk({ delta: 'a', seq: 1 }));
		aggregator.chunkHandler(true, undefined)(chunk({ delta: 'b', seq: 2 }));

		aggregator.flushChunksNow();

		expect(dispatch).toHaveBeenCalledTimes(2);
		expect(dispatch.mock.calls.map((call) => call[0].type)).toEqual([
			'agent/chunks',
			'agent/chunks',
		]);
		expect(dispatch.mock.calls.map((call) => call[0].chunks[0].payload.delta)).toEqual([
			'a',
			'b',
		]);
		expect(aggregator.metricsSnapshot()).toEqual({ frames: 0, chunks: 2, drops: 0 });
	});

	it('counts one frame for a real multi-session animation-frame callback', () => {
		const aggregator = createAggregator();
		aggregator.chunkHandler(true, undefined)(chunk({ sessionId: 'ses-a', messageId: 'a' }));
		aggregator.chunkHandler(true, undefined)(chunk({ sessionId: 'ses-b', messageId: 'b' }));

		frame?.(0);

		expect(aggregator.metricsSnapshot()).toEqual({ frames: 1, chunks: 2, drops: 0 });
	});

	it('does not count a cancelled RAF that was replaced by a manual flush', () => {
		const aggregator = createAggregator();
		aggregator.chunkHandler(true, undefined)(chunk());
		aggregator.flushChunksNow();

		expect(aggregator.metricsSnapshot().frames).toBe(0);
	});

	it('cancels a stale RAF before painting the active step first chunk', () => {
		const aggregator = createAggregator();
		aggregator.chunkHandler(
			true,
			undefined,
		)(chunk({ sessionId: 'ses-other', messageId: 'other', stepNumber: 1 }));
		expect(frame).not.toBeNull();

		aggregator.chunkHandler(
			true,
			undefined,
		)(chunk({ sessionId: 'ses-stream-test', messageId: 'active', stepNumber: 2 }));

		expect(vi.mocked(cancelAnimationFrame)).toHaveBeenCalledWith(1);
	});
});
