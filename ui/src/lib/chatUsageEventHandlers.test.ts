import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';
import { createChatUsageEventHandlers } from './chatUsageEventHandlers.ts';
import { sessionLlmUsageStore, sessionTokenStatsStore } from './sessionUsage.ts';

describe('createChatUsageEventHandlers', () => {
	beforeEach(() => {
		sessionLlmUsageStore.set({});
		sessionTokenStatsStore.set({});
	});

	it('keeps ingress media and other tool usage out of Agent totals', () => {
		const handle = createChatUsageEventHandlers()['agent:usage'];
		for (const [id, callKind] of [
			['media', 'media'],
			['tool', 'tool'],
		] as const) {
			handle({
				event: 'agent:usage',
				id: id === 'media' ? 1 : 2,
				payload: {
					sessionId: 'ses-test',
					promptTokens: 11,
					completionTokens: 7,
					totalTokens: 18,
					cachedTokens: 0,
					cacheCreationTokens: 0,
					cacheMissTokens: 11,
					contextTokens: 11,
					cacheExclusive: false,
					cacheAccounting: 'unknown',
					costUsd: null,
					model: 'test-model',
					cumulativePromptTokens: 0,
					cumulativeCompletionTokens: 0,
					cumulativeTotalTokens: 0,
					cumulativeCachedTokens: 0,
					cumulativeCacheCreationTokens: 0,
					cumulativeCacheMissTokens: 0,
					cumulativeCostUsd: null,
					contextWindow: null,
					role: 'default_model',
					callKind,
					hasCost: false,
				},
			} as any);
		}

		expect(get(sessionTokenStatsStore)['ses-test']).toBeUndefined();
		expect(get(sessionLlmUsageStore)['ses-test']).toEqual([
			expect.objectContaining({ call_kind: 'media', step_number: null }),
			expect.objectContaining({ call_kind: 'tool', step_number: null }),
		]);
	});
});
