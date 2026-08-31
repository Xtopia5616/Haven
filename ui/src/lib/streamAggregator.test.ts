import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import type { AgentChunkPayload } from './contracts/agent.ts';
import { sessionMessagesStore } from './sessionMessages.ts';
import {
	createStreamEventAggregator,
	foldContiguousChunks,
	type PendingChunk,
} from './streamAggregator.ts';

describe('createStreamEventAggregator', () => {
	let frame: FrameRequestCallback | null = null;

	beforeEach(() => {
		sessionMessagesStore.set({});
		frame = null;
		vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
			frame = callback;
			return 1;
		});
		vi.stubGlobal('cancelAnimationFrame', vi.fn());
	});

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

	it('deduplicates sequence replays before the frame flush', () => {
		const onActiveStream = vi.fn();
		const aggregator = createStreamEventAggregator({
			getActiveSessionId: () => 'ses-stream-test',
			onActiveStream,
		});
		const handler = aggregator.chunkHandler(true, undefined);

		handler(chunk());
		handler(chunk({ delta: 'duplicate', seq: 1 }));
		expect(get(sessionMessagesStore)).toEqual({});
		expect(onActiveStream).toHaveBeenCalledTimes(2);

		aggregator.flushChunksNow();
		expect(get(sessionMessagesStore)['ses-stream-test']).toHaveLength(1);
		expect(get(sessionMessagesStore)['ses-stream-test'][0].content).toBe('hello');
	});

	it('finalizes reasoning before applying the thought chunk for the same step', () => {
		const aggregator = createStreamEventAggregator({
			getActiveSessionId: () => 'ses-stream-test',
			onActiveStream: vi.fn(),
		});
		aggregator.chunkHandler(false, 'reasoning')(
			chunk({ messageId: 'step-reasoning-test', delta: 'reason', seq: 1 }),
		);
		aggregator.chunkHandler(true, undefined)(
			chunk({ messageId: 'step-thought-test-2', delta: 'thought', seq: 1 }),
		);

		expect(frame).not.toBeNull();
		aggregator.flushChunksNow();
		const messages = get(sessionMessagesStore)['ses-stream-test'];
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
});
