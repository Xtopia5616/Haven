import { beforeEach, describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { clearMediaPlans, mediaPlanStore, rememberMediaPlan } from './mediaPlanStore.ts';

const sessionId = (value: number) => `ses-${value.toString(16).padStart(32, '0')}`;

function plan(session: number, step: number) {
	return {
		sessionId: sessionId(session),
		stepNumber: step,
		runId: 1,
		requestKind: 'chat' as const,
		strategy: 'text_only_safe' as const,
		projections: [],
		notices: [],
	};
}

describe('media plan store', () => {
	beforeEach(() => mediaPlanStore.set({}));

	it('bounds each session and the global session cache while refreshing recency', () => {
		for (let step = 1; step <= 34; step += 1) rememberMediaPlan(plan(1, step));
		expect(get(mediaPlanStore)[sessionId(1)]).toHaveLength(32);
		expect(get(mediaPlanStore)[sessionId(1)]?.[0]?.stepNumber).toBe(3);

		for (let session = 2; session <= 33; session += 1) {
			rememberMediaPlan(plan(session, 1));
		}
		rememberMediaPlan(plan(1, 35));
		rememberMediaPlan(plan(34, 1));

		const stored = get(mediaPlanStore);
		expect(Object.keys(stored)).toHaveLength(32);
		expect(stored[sessionId(1)]?.at(-1)?.stepNumber).toBe(35);
		expect(stored[sessionId(2)]).toBeUndefined();
		expect(stored[sessionId(34)]).toHaveLength(1);
	});

	it('clears one session or the complete cache', () => {
		rememberMediaPlan(plan(1, 1));
		rememberMediaPlan(plan(2, 1));
		clearMediaPlans(sessionId(1));
		expect(Object.keys(get(mediaPlanStore))).toEqual([sessionId(2)]);
		clearMediaPlans(null);
		expect(get(mediaPlanStore)).toEqual({});
	});
});
