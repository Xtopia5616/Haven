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
			getActiveSessionId: () => 'ses-6751ce00136ce79085a12dc820ec747c',
			onActiveStream: vi.fn(),
			dispatch,
			getBlockIds: (sessionId, stepNumber, runId) =>
				reducer.getBlockIds(sessionId, stepNumber, runId),
		});
	}

	function chunk(overrides: Partial<AgentChunkPayload> = {}): { payload: AgentChunkPayload } {
		return {
			payload: {
				sessionId: 'ses-6751ce00136ce79085a12dc820ec747c',
				delta: 'hello',
				stepNumber: 1,
				runId: 1,
				messageId: 'step-ce193f30dde770e8c33e31e94b42f31a',
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
		expect(reducer.getMessages('ses-6751ce00136ce79085a12dc820ec747c')).toHaveLength(1);
		expect(reducer.getMessages('ses-6751ce00136ce79085a12dc820ec747c')[0].content).toBe('hello');

		aggregator.flushChunksNow();
		expect(reducer.getMessages('ses-6751ce00136ce79085a12dc820ec747c')).toHaveLength(1);
		expect(reducer.getMessages('ses-6751ce00136ce79085a12dc820ec747c')[0].content).toBe('hello');
		expect(aggregator.metricsSnapshot()).toEqual({ frames: 0, chunks: 2, drops: 0 });
	});

	it('dispatches a thought frame after the reducer finalizes reasoning', () => {
		const aggregator = createAggregator();
		aggregator.chunkHandler(
			false,
			'reasoning',
		)(chunk({ messageId: 'step-cfab4805314410f1e0bdda7d8557047b', delta: 'reason', seq: 1 }));
		aggregator.chunkHandler(
			true,
			undefined,
		)(chunk({ messageId: 'step-6e836ca15cb739d7ee23e1060e098217', delta: 'thought', seq: 1 }));

		aggregator.flushChunksNow();
		const messages = reducer.getMessages('ses-6751ce00136ce79085a12dc820ec747c');
		expect(messages.map((message) => [message.content, message.streaming])).toEqual([
			['reason', false],
			['thought', true],
		]);
	});

	it('keeps interleaved stream blocks in arrival order', () => {
		const base: PendingChunk = {
			tid: 'ses-6751ce00136ce79085a12dc820ec747c',
			sid: 'step-c0b40676b839848be9143e9057e10785',
			delta: '',
			msgType: undefined,
			stepNumber: 1,
			runId: 1,
			time: 'now',
			finalizeReasoning: true,
		};
		const folded = foldContiguousChunks([
			{ ...base, delta: 'A1' },
			{ ...base, sid: 'step-5b45e3c7e661d88e9cbf98c591ac32e8', delta: 'B1', finalizeReasoning: false },
			{ ...base, delta: 'A2' },
		]);

		expect(folded.map(({ sid, delta }) => [sid, delta])).toEqual([
			['step-c0b40676b839848be9143e9057e10785', 'A1'],
			['step-5b45e3c7e661d88e9cbf98c591ac32e8', 'B1'],
			['step-c0b40676b839848be9143e9057e10785', 'A2'],
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
		aggregator.chunkHandler(true, undefined)(chunk({ sessionId: 'ses-32f23b1e00f85571a60f75dde5588074', messageId: 'a' }));
		aggregator.chunkHandler(true, undefined)(chunk({ sessionId: 'ses-b77487af9bb14e5ec4d60572f1248bce', messageId: 'b' }));

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
		)(chunk({ sessionId: 'ses-fb38531de0466990f46b178209103744', messageId: 'other', stepNumber: 1 }));
		expect(frame).not.toBeNull();

		aggregator.chunkHandler(
			true,
			undefined,
		)(chunk({ sessionId: 'ses-6751ce00136ce79085a12dc820ec747c', messageId: 'active', stepNumber: 2 }));

		expect(vi.mocked(cancelAnimationFrame)).toHaveBeenCalledWith(1);
	});
});
