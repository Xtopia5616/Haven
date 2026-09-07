import { describe, expect, it } from 'vitest';
import { isBootstrapReady, nextBootstrapProbeInterval } from './bootstrapStatus.ts';

describe('bootstrapStatus', () => {
	it('only treats the backend ready status as complete', () => {
		expect(isBootstrapReady('loading')).toBe(false);
		expect(isBootstrapReady('ready')).toBe(true);
		expect(isBootstrapReady(undefined)).toBe(false);
	});

	it('backs off failed readiness probes with a bounded delay', () => {
		expect(nextBootstrapProbeInterval(0)).toBe(1000);
		expect(nextBootstrapProbeInterval(2)).toBe(4000);
		expect(nextBootstrapProbeInterval(99)).toBe(10000);
	});
});
