import { describe, expect, it } from 'vitest';
import { buildSessionSwitcherOptions } from './sessionSwitcher.ts';
import type { SessionHistoryRow } from './contracts/sessionHistory.ts';
import type { SessionSummary } from './sessionReducer/types.ts';

describe('buildSessionSwitcherOptions', () => {
	it('keeps completed history switchable and prefers live summaries for duplicates', () => {
		const live: SessionSummary[] = [
			{ sessionId: 'ses-1bd4a9f01b95da1dfb71021b982a09a8', status: 'running', title: '正在绘画' },
		];
		const history = [
			{
				sessionId: 'ses-76f19db031ba6b93089e8acb9f43666f',
				input_text: '旧绘画会话',
				title: null,
				status: 'completed',
			},
			{
				sessionId: 'ses-1bd4a9f01b95da1dfb71021b982a09a8',
				input_text: '已完成的旧快照',
				title: null,
				status: 'completed',
			},
		] as SessionHistoryRow[];

		const options = buildSessionSwitcherOptions(live, history);

		expect(options.map((session) => session.sessionId)).toEqual(['ses-1bd4a9f01b95da1dfb71021b982a09a8', 'ses-76f19db031ba6b93089e8acb9f43666f']);
		expect(options[0]).toMatchObject({ sessionId: 'ses-1bd4a9f01b95da1dfb71021b982a09a8', status: 'running' });
		expect(options[1]).toMatchObject({
			sessionId: 'ses-76f19db031ba6b93089e8acb9f43666f',
			status: 'completed',
			title: '旧绘画会话',
			inputText: '旧绘画会话',
		});
		expect(options[1]).not.toHaveProperty('input_text');
	});
});
