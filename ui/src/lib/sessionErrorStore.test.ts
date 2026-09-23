import { afterEach, describe, expect, it } from 'vitest';
import { SessionReducer } from './sessionReducer.ts';
import {
	forgetSessionError,
	getSessionErrorReason,
	rememberSessionError,
} from './sessionErrorStore.ts';

const cachedSessionIds = [
	'ses-00000000000000000000000000000001',
	'ses-00000000000000000000000000000002',
	'ses-00000000000000000000000000000003',
];

afterEach(() => {
	for (const sessionId of cachedSessionIds) forgetSessionError(sessionId);
});

describe('sessionErrorStore compatibility facade', () => {
	it('trims reasons, isolates sessions, and treats normalized repeats as no-ops', () => {
		rememberSessionError(cachedSessionIds[0], '  网络失败  ');
		rememberSessionError(cachedSessionIds[1], '  超时  ');
		const afterFirstWrite = new SessionReducer();
		afterFirstWrite.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: cachedSessionIds[2],
			reason: '原因',
		});
		const unchangedState = afterFirstWrite.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: cachedSessionIds[2],
			reason: '  原因  ',
		});

		expect(getSessionErrorReason(cachedSessionIds[0])).toBe('网络失败');
		expect(getSessionErrorReason(cachedSessionIds[1])).toBe('超时');
		expect(afterFirstWrite.getSessionErrorReason(cachedSessionIds[2])).toBe('原因');
		expect(unchangedState).toBe(afterFirstWrite.getState());
	});

	it('ignores empty session ids and blank reasons without replacing a cached reason', () => {
		rememberSessionError(cachedSessionIds[0], '已有原因');
		rememberSessionError(cachedSessionIds[0], '   ');
		rememberSessionError('', '无 session 的原因');

		expect(getSessionErrorReason(cachedSessionIds[0])).toBe('已有原因');
		expect(getSessionErrorReason('')).toBe('');
	});

	it('forgets only the requested session and returns empty for unknown sessions', () => {
		rememberSessionError(cachedSessionIds[0], '原因 A');
		rememberSessionError(cachedSessionIds[1], '原因 B');

		forgetSessionError(cachedSessionIds[0]);
		forgetSessionError(cachedSessionIds[0]);

		expect(getSessionErrorReason(cachedSessionIds[0])).toBe('');
		expect(getSessionErrorReason(cachedSessionIds[1])).toBe('原因 B');
		expect(getSessionErrorReason('ses-000000000000000000000000000000ff')).toBe('');
	});

	it('retains cached reasons across session deletion and list clearing', () => {
		const reducer = new SessionReducer();
		reducer.dispatch({
			type: 'session/error-reason-remembered',
			sessionId: cachedSessionIds[2],
			reason: '历史原因',
		});

		reducer.dispatch({ type: 'session/deleted', sessionId: cachedSessionIds[2] });
		expect(reducer.getSessionErrorReason(cachedSessionIds[2])).toBe('历史原因');

		reducer.dispatch({ type: 'sessions/cleared' });
		expect(reducer.getSessionErrorReason(cachedSessionIds[2])).toBe('历史原因');
	});
});
