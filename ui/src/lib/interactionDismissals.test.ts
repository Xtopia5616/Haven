import { describe, expect, it } from 'vitest';
import { retainPendingDismissedRequestIds } from './interactionDismissals.ts';

describe('retainPendingDismissedRequestIds', () => {
	it('keeps dismissed requests while they remain pending', () => {
		expect(retainPendingDismissedRequestIds(new Set(['conf-live']), ['conf-live'])).toBeNull();
	});

	it('drops resolved or removed requests while preserving other pending dismissals', () => {
		expect(
			retainPendingDismissedRequestIds(
				new Set(['ask-resolved', 'conf-live']),
				['conf-live', 'ask-new'],
			),
		).toEqual(new Set(['conf-live']));
	});

	it('returns null for an empty dismissal set', () => {
		expect(retainPendingDismissedRequestIds(new Set(), [])).toBeNull();
	});
});
